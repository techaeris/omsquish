use crate::compressor::{compress_image_data, compute_output_destination};
use crate::sample_data::generate_sample_images;
use crate::types::{
    CompressionMode, CompressionSettings, ImageFormatType, OutputFormat, ProcessStatus, QueueItem,
    ResizeMode,
};
use eframe::egui::{self, Color32, FontId, RichText, Stroke, TextureHandle, Vec2};
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

#[derive(Debug, PartialEq)]
enum SettingsTab {
    Compression,
    Resize,
    Output,
}

#[derive(Debug, Clone)]
pub struct ItemCompressionOutcome {
    pub compressed_bytes: Vec<u8>,
    pub compressed_size: u64,
    pub dimensions: (u32, u32),
    pub output_format: ImageFormatType,
    pub output_path: Option<PathBuf>,
    pub skipped_reason: Option<String>,
    pub compressed_preview: Option<egui::ColorImage>,
}

pub enum AppMessage {
    FilesLoaded(Vec<QueueItem>),
    OutputFolderSelected(PathBuf),
    ItemStarted(usize),
    ItemCompressed {
        index: usize,
        result: Result<ItemCompressionOutcome, String>,
    },
    AllCompressionFinished,
    OriginalPreviewLoaded {
        index: usize,
        bytes: Vec<u8>,
        color_image: egui::ColorImage,
    },
    CompressedPreviewLoaded {
        index: usize,
        color_image: egui::ColorImage,
    },
    DialogClosed,
}

pub fn is_supported_image(path: &std::path::Path) -> bool {
    let ext = match path.extension().and_then(|s| s.to_str()) {
        Some(e) => e.to_lowercase(),
        None => return false,
    };
    matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "webp")
}

pub fn decode_percent(input: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(input.len());
    let mut chars = input.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let h1 = chars.next();
            let h2 = chars.next();
            if let (Some(h1), Some(h2)) = (h1, h2) {
                let hex_str = [h1, h2];
                if let Ok(hex_val) = std::str::from_utf8(&hex_str) {
                    if let Ok(byte) = u8::from_str_radix(hex_val, 16) {
                        bytes.push(byte);
                        continue;
                    }
                }
                bytes.push(b'%');
                bytes.push(h1);
                bytes.push(h2);
            } else {
                bytes.push(b'%');
                if let Some(h1) = h1 {
                    bytes.push(h1);
                }
            }
        } else {
            bytes.push(b);
        }
    }
    bytes
}

pub fn parse_uri_list_or_paths(raw: &str) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for raw_line in raw.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let unquoted = if (line.starts_with('"') && line.ends_with('"'))
            || (line.starts_with('\'') && line.ends_with('\''))
        {
            if line.len() >= 2 {
                &line[1..line.len() - 1]
            } else {
                line
            }
        } else {
            line
        };

        let path_str = if let Some(stripped) = unquoted.strip_prefix("file://localhost") {
            stripped
        } else if let Some(stripped) = unquoted.strip_prefix("file://") {
            stripped
        } else if let Some(stripped) = unquoted.strip_prefix("file:") {
            stripped
        } else {
            unquoted
        };

        let decoded_bytes = decode_percent(path_str);
        #[cfg(unix)]
        {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt;
            let os_str = OsString::from_vec(decoded_bytes);
            let path = PathBuf::from(os_str);
            if !path.as_os_str().is_empty() {
                result.push(path);
            }
        }
        #[cfg(not(unix))]
        {
            if let Ok(utf8_str) = String::from_utf8(decoded_bytes) {
                let path = PathBuf::from(utf8_str);
                if !path.as_os_str().is_empty() {
                    result.push(path);
                }
            }
        }
    }
    result
}

pub fn draw_dashed_rect(
    painter: &egui::Painter,
    rect: egui::Rect,
    color: Color32,
    dash_length: f32,
    gap_length: f32,
    stroke_width: f32,
) {
    if rect.width() < 10.0 || rect.height() < 10.0 {
        return;
    }
    let stroke = egui::Stroke::new(stroke_width, color);
    let draw_dashed_line = |start: egui::Pos2, end: egui::Pos2| {
        let dir = (end - start).normalized();
        let total_dist = start.distance(end);
        let mut dist = 0.0;
        while dist < total_dist {
            let next_dist = (dist + dash_length).min(total_dist);
            let p1 = start + dir * dist;
            let p2 = start + dir * next_dist;
            painter.line_segment([p1, p2], stroke);
            dist += dash_length + gap_length;
        }
    };

    draw_dashed_line(rect.left_top(), rect.right_top());
    draw_dashed_line(rect.right_top(), rect.right_bottom());
    draw_dashed_line(rect.right_bottom(), rect.left_bottom());
    draw_dashed_line(rect.left_bottom(), rect.left_top());
}

pub fn load_files_background(paths: Vec<PathBuf>) -> Vec<QueueItem> {
    use rayon::prelude::*;
    paths
        .into_par_iter()
        .filter_map(|path| {
            if !path.is_file() || !is_supported_image(&path) {
                return None;
            }
            let filename = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "image".to_string());
            let ext = path
                .extension()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let format = ImageFormatType::from_extension(&ext);

            let meta = fs::metadata(&path);
            let size = meta.map(|m| m.len()).unwrap_or(0);

            let dimensions = match image::image_dimensions(&path) {
                Ok(dim) => dim,
                Err(_) => (0, 0),
            };

            Some(QueueItem {
                id: 0,
                file_path: Some(path),
                filename,
                format,
                original_size: size,
                original_dimensions: dimensions,
                compressed_size: None,
                compressed_dimensions: None,
                status: ProcessStatus::Ready,
                output_path: None,
                preview_original_bytes: None,
                preview_compressed_bytes: None,
            })
        })
        .collect()
}

#[derive(serde::Deserialize)]
struct HyprWorkspace {
    id: i32,
}

#[derive(serde::Deserialize)]
struct HyprMonitor {
    #[serde(rename = "activeWorkspace")]
    active_workspace: HyprWorkspace,
}

#[derive(serde::Deserialize)]
struct HyprClient {
    pid: u32,
    workspace: HyprWorkspace,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    class: String,
    #[serde(default, rename = "initialClass")]
    initial_class: String,
}

fn check_hyprland_window_visible(my_pid: u32) -> Option<bool> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let xdg = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".to_string());
    let sock_path = format!("{}/hypr/{}/.socket.sock", xdg, sig);

    let mut stream = UnixStream::connect(&sock_path).ok()?;
    stream.set_read_timeout(Some(std::time::Duration::from_millis(20))).ok();
    stream.set_write_timeout(Some(std::time::Duration::from_millis(20))).ok();

    stream.write_all(b"j/monitors").ok()?;
    let mut monitors_buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                monitors_buf.extend_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let monitors: Vec<HyprMonitor> = serde_json::from_slice(&monitors_buf).ok()?;
    let active_workspaces: HashSet<i32> = monitors
        .into_iter()
        .map(|m| m.active_workspace.id)
        .collect();

    let mut stream = UnixStream::connect(&sock_path).ok()?;
    stream.set_read_timeout(Some(std::time::Duration::from_millis(20))).ok();
    stream.set_write_timeout(Some(std::time::Duration::from_millis(20))).ok();

    stream.write_all(b"j/clients").ok()?;
    let mut clients_buf = Vec::new();
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                clients_buf.extend_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let clients: Vec<HyprClient> = serde_json::from_slice(&clients_buf).ok()?;
    for c in clients {
        let is_match = c.pid == my_pid
            || c.class.to_lowercase().contains("omsquish")
            || c.initial_class.to_lowercase().contains("omsquish")
            || c.class.to_lowercase().contains("imagesquish")
            || c.initial_class.to_lowercase().contains("imagesquish");
        if is_match {
            let on_active_ws = active_workspaces.contains(&c.workspace.id);
            return Some(!c.hidden && on_active_ws);
        }
    }

    None
}

pub struct OmSquishApp {
    pub queue: Vec<QueueItem>,
    pub selected_index: Option<usize>,
    pub settings: CompressionSettings,
    active_tab: SettingsTab,
    pub show_preview: bool,
    pub is_processing: bool,
    pub progress: f32,
    pub is_dialog_open: bool,
    original_preview_loading: Option<usize>,
    compressed_preview_loading: Option<usize>,

    // Channels for background tasks
    message_sender: Sender<AppMessage>,
    message_receiver: Receiver<AppMessage>,

    // Textures for preview
    original_texture: Option<TextureHandle>,
    original_texture_key: Option<usize>,
    compressed_texture: Option<TextureHandle>,
    compressed_texture_key: Option<usize>,
    app_icon_texture: Option<TextureHandle>,

    // For automated testing & screenshots
    pub screenshot_path: Option<String>,
    pub frame_counter: u32,

    // Window visibility & occlusion tracking (for Hyprland / Wayland)
    last_visibility_check: std::time::Instant,
    is_window_visible: bool,

    // Visual drop-zone hover state
    pub is_drag_hovering: bool,
}

impl OmSquishApp {
    pub fn new(screenshot_path: Option<String>) -> Self {
        let sample_queue = generate_sample_images();
        let (message_sender, message_receiver) = channel();
        Self {
            queue: sample_queue,
            selected_index: Some(0),
            settings: CompressionSettings::default(),
            active_tab: SettingsTab::Compression,
            show_preview: true,
            is_processing: false,
            progress: 0.0,
            is_dialog_open: false,
            original_preview_loading: None,
            compressed_preview_loading: None,
            message_sender,
            message_receiver,
            original_texture: None,
            original_texture_key: None,
            compressed_texture: None,
            compressed_texture_key: None,
            app_icon_texture: None,
            screenshot_path,
            frame_counter: 0,
            last_visibility_check: std::time::Instant::now(),
            is_window_visible: true,
            is_drag_hovering: false,
        }
    }

    pub fn format_bytes(bytes: u64) -> String {
        const KB: u64 = 1024;
        const MB: u64 = KB * 1024;
        const GB: u64 = MB * 1024;

        if bytes >= GB {
            format!("{:.2} GB", bytes as f64 / GB as f64)
        } else if bytes >= MB {
            format!("{:.2} MB", bytes as f64 / MB as f64)
        } else if bytes >= KB {
            format!("{:.1} KB", bytes as f64 / KB as f64)
        } else {
            format!("{} B", bytes)
        }
    }

    pub fn spawn_add_files(&mut self, ctx: &egui::Context) {
        if self.is_dialog_open {
            return;
        }
        self.is_dialog_open = true;
        let tx = self.message_sender.clone();
        let ctx_clone = ctx.clone();
        let existing_paths: HashSet<PathBuf> = self
            .queue
            .iter()
            .filter_map(|item| item.file_path.clone())
            .collect();

        std::thread::spawn(move || {
            let picked = pollster::block_on(async {
                rfd::AsyncFileDialog::new()
                    .add_filter(
                        "Images",
                        &["jpg", "jpeg", "png", "webp"],
                    )
                    .pick_files()
                    .await
            });

            if let Some(files) = picked {
                let paths: Vec<PathBuf> =
                    files.into_iter().map(|f| f.path().to_path_buf()).collect();
                let mut files_to_load = Vec::new();
                let mut seen_paths = existing_paths;
                for p in paths {
                    if is_supported_image(&p) {
                        let norm = p.canonicalize().unwrap_or_else(|_| p.clone());
                        if !seen_paths.contains(&p) && !seen_paths.contains(&norm) {
                            seen_paths.insert(p.clone());
                            seen_paths.insert(norm);
                            files_to_load.push(p);
                        }
                    }
                }
                if files_to_load.is_empty() {
                    let _ = tx.send(AppMessage::DialogClosed);
                } else {
                    let items = load_files_background(files_to_load);
                    let _ = tx.send(AppMessage::FilesLoaded(items));
                }
            } else {
                let _ = tx.send(AppMessage::DialogClosed);
            }
            ctx_clone.request_repaint();
        });
    }

    pub fn spawn_add_folder(&mut self, ctx: &egui::Context) {
        if self.is_dialog_open {
            return;
        }
        self.is_dialog_open = true;
        let tx = self.message_sender.clone();
        let ctx_clone = ctx.clone();
        let existing_paths: HashSet<PathBuf> = self
            .queue
            .iter()
            .filter_map(|item| item.file_path.clone())
            .collect();

        std::thread::spawn(move || {
            let picked = pollster::block_on(async {
                rfd::AsyncFileDialog::new().pick_folder().await
            });
            if let Some(folder_handle) = picked {
                let folder = folder_handle.path().to_path_buf();
                let mut files_to_load = Vec::new();
                let mut seen_paths = existing_paths;
                for entry in walkdir::WalkDir::new(folder)
                    .follow_links(true)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    if entry.file_type().is_file() {
                        let p = entry.path().to_path_buf();
                        if is_supported_image(&p) {
                            let norm = p.canonicalize().unwrap_or_else(|_| p.clone());
                            if !seen_paths.contains(&p) && !seen_paths.contains(&norm) {
                                seen_paths.insert(p.clone());
                                seen_paths.insert(norm);
                                files_to_load.push(p);
                            }
                        }
                    }
                }
                if files_to_load.is_empty() {
                    let _ = tx.send(AppMessage::DialogClosed);
                } else {
                    let items = load_files_background(files_to_load);
                    let _ = tx.send(AppMessage::FilesLoaded(items));
                }
            } else {
                let _ = tx.send(AppMessage::DialogClosed);
            }
            ctx_clone.request_repaint();
        });
    }

    pub fn spawn_browse_output_folder(&mut self, ctx: &egui::Context) {
        if self.is_dialog_open {
            return;
        }
        self.is_dialog_open = true;
        let tx = self.message_sender.clone();
        let ctx_clone = ctx.clone();
        std::thread::spawn(move || {
            let picked = pollster::block_on(async {
                rfd::AsyncFileDialog::new().pick_folder().await
            });
            if let Some(folder_handle) = picked {
                let folder = folder_handle.path().to_path_buf();
                let _ = tx.send(AppMessage::OutputFolderSelected(folder));
            } else {
                let _ = tx.send(AppMessage::DialogClosed);
            }
            ctx_clone.request_repaint();
        });
    }

    pub fn add_files_async(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        if paths.is_empty() {
            return;
        }
        self.is_dialog_open = true;

        let existing_paths: HashSet<PathBuf> = self
            .queue
            .iter()
            .filter_map(|item| item.file_path.clone())
            .collect();

        let tx = self.message_sender.clone();
        let ctx_clone = ctx.clone();

        std::thread::spawn(move || {
            let mut files_to_load = Vec::new();
            let mut seen_paths = existing_paths;

            for path in paths {
                if path.is_dir() {
                    for entry in walkdir::WalkDir::new(&path)
                        .follow_links(true)
                        .into_iter()
                        .filter_map(|e| e.ok())
                    {
                        if entry.file_type().is_file() {
                            let p = entry.path().to_path_buf();
                            if is_supported_image(&p) {
                                let norm = p.canonicalize().unwrap_or_else(|_| p.clone());
                                if !seen_paths.contains(&p) && !seen_paths.contains(&norm) {
                                    seen_paths.insert(p.clone());
                                    seen_paths.insert(norm);
                                    files_to_load.push(p);
                                }
                            }
                        }
                    }
                } else if path.is_file() {
                    if is_supported_image(&path) {
                        let norm = path.canonicalize().unwrap_or_else(|_| path.clone());
                        if !seen_paths.contains(&path) && !seen_paths.contains(&norm) {
                            seen_paths.insert(path.clone());
                            seen_paths.insert(norm);
                            files_to_load.push(path);
                        }
                    }
                }
            }

            if files_to_load.is_empty() {
                let _ = tx.send(AppMessage::DialogClosed);
            } else {
                let items = load_files_background(files_to_load);
                let _ = tx.send(AppMessage::FilesLoaded(items));
            }
            ctx_clone.request_repaint();
        });
    }

    pub fn start_compress_all(&mut self, ctx: &egui::Context) {
        if self.is_processing || self.queue.is_empty() {
            return;
        }
        self.is_processing = true;
        self.progress = 0.0;

        // Reset non-finished items so live progress is apparent
        for item in &mut self.queue {
            if !matches!(item.status, ProcessStatus::Finished) {
                item.status = ProcessStatus::Ready;
            }
        }

        let tx = self.message_sender.clone();
        let ctx_clone = ctx.clone();
        let settings = self.settings.clone();
        let current_selected = self.selected_index;

        struct CompressTask {
            index: usize,
            file_path: Option<PathBuf>,
            preview_bytes: Option<Vec<u8>>,
            format: ImageFormatType,
            original_size: u64,
            original_dimensions: (u32, u32),
        }

        let tasks: Vec<CompressTask> = self
            .queue
            .iter()
            .enumerate()
            .map(|(i, q)| CompressTask {
                index: i,
                file_path: q.file_path.clone(),
                preview_bytes: q.preview_original_bytes.clone(),
                format: q.format.clone(),
                original_size: q.original_size,
                original_dimensions: q.original_dimensions,
            })
            .collect();

        std::thread::spawn(move || {
            for task in tasks {
                // Signal UI that this item is currently being compressed
                let _ = tx.send(AppMessage::ItemStarted(task.index));
                ctx_clone.request_repaint();

                let bytes = if let Some(ref p) = task.file_path {
                    match fs::read(p) {
                        Ok(b) => b,
                        Err(e) => {
                            let _ = tx.send(AppMessage::ItemCompressed {
                                index: task.index,
                                result: Err(format!("Read error: {}", e)),
                            });
                            ctx_clone.request_repaint();
                            continue;
                        }
                    }
                } else if let Some(ref b) = task.preview_bytes {
                    b.clone()
                } else {
                    let _ = tx.send(AppMessage::ItemCompressed {
                        index: task.index,
                        result: Err("No image data available".to_string()),
                    });
                    ctx_clone.request_repaint();
                    continue;
                };

                match compress_image_data(&bytes, &task.format, &settings) {
                    Ok(res) => {
                        let comp_len = res.compressed_bytes.len() as u64;

                        // Check skip if bigger
                        if settings.skip_if_bigger && comp_len >= task.original_size {
                            let _ = tx.send(AppMessage::ItemCompressed {
                                index: task.index,
                                result: Ok(ItemCompressionOutcome {
                                    compressed_bytes: res.compressed_bytes,
                                    compressed_size: task.original_size,
                                    dimensions: task.original_dimensions,
                                    output_format: res.output_format,
                                    output_path: None,
                                    skipped_reason: Some("Output larger than original".to_string()),
                                    compressed_preview: None,
                                }),
                            });
                            ctx_clone.request_repaint();
                            continue;
                        }

                        // Perform file saving completely on background thread
                        let mut saved_dest = None;
                        if let Some(ref p) = task.file_path {
                            if let Some(dest) = compute_output_destination(
                                Some(p),
                                &settings,
                                &res.output_format,
                            ) {
                                if let Some(parent) = dest.parent() {
                                    let _ = fs::create_dir_all(parent);
                                }
                                if let Err(e) = fs::write(&dest, &res.compressed_bytes) {
                                    let _ = tx.send(AppMessage::ItemCompressed {
                                        index: task.index,
                                        result: Err(format!("Failed to save: {}", e)),
                                    });
                                    ctx_clone.request_repaint();
                                    continue;
                                } else {
                                    saved_dest = Some(dest);
                                }
                            }
                        }

                        // Decode preview on background worker thread if this item is currently selected
                        let compressed_preview = if current_selected == Some(task.index) {
                            if let Ok(img) = image::load_from_memory(&res.compressed_bytes) {
                                let img = if img.width() > 1280 || img.height() > 1280 {
                                    img.thumbnail(1280, 1280)
                                } else {
                                    img
                                };
                                let rgba = img.to_rgba8();
                                let (w, h) = rgba.dimensions();
                                Some(egui::ColorImage::from_rgba_unmultiplied(
                                    [w as usize, h as usize],
                                    &rgba.into_raw(),
                                ))
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                        let _ = tx.send(AppMessage::ItemCompressed {
                            index: task.index,
                            result: Ok(ItemCompressionOutcome {
                                compressed_bytes: res.compressed_bytes,
                                compressed_size: comp_len,
                                dimensions: res.dimensions,
                                output_format: res.output_format,
                                output_path: saved_dest,
                                skipped_reason: None,
                                compressed_preview,
                            }),
                        });
                        ctx_clone.request_repaint();
                    }
                    Err(e) => {
                        let _ = tx.send(AppMessage::ItemCompressed {
                            index: task.index,
                            result: Err(e),
                        });
                        ctx_clone.request_repaint();
                    }
                }
            }

            let _ = tx.send(AppMessage::AllCompressionFinished);
            ctx_clone.request_repaint();
        });
    }

    fn update_textures(&mut self, ctx: &egui::Context) {
        if let Some(idx) = self.selected_index {
            if idx < self.queue.len() {
                // Check if original texture needs updating - ALL decoding runs on background thread
                if self.original_texture_key != Some(idx)
                    && self.original_preview_loading != Some(idx)
                {
                    self.original_preview_loading = Some(idx);
                    let tx = self.message_sender.clone();
                    let ctx_clone = ctx.clone();
                    let file_path = self.queue[idx].file_path.clone();
                    let preview_bytes = self.queue[idx].preview_original_bytes.clone();

                    std::thread::spawn(move || {
                        let bytes = if let Some(b) = preview_bytes {
                            Some(b)
                        } else if let Some(ref p) = file_path {
                            fs::read(p).ok()
                        } else {
                            None
                        };

                        if let Some(bytes) = bytes {
                            if let Ok(img) = image::load_from_memory(&bytes) {
                                let img = if img.width() > 1280 || img.height() > 1280 {
                                    img.thumbnail(1280, 1280)
                                } else {
                                    img
                                };
                                let rgba = img.to_rgba8();
                                let (w, h) = rgba.dimensions();
                                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                                    [w as usize, h as usize],
                                    &rgba.into_raw(),
                                );
                                let _ = tx.send(AppMessage::OriginalPreviewLoaded {
                                    index: idx,
                                    bytes,
                                    color_image,
                                });
                                ctx_clone.request_repaint();
                            }
                        }
                    });
                }

                // Check if compressed texture needs updating - ALL decoding runs on background thread
                if self.compressed_texture_key != Some(idx) {
                    if let Some(ref bytes) = self.queue[idx].preview_compressed_bytes {
                        if self.compressed_preview_loading != Some(idx) {
                            self.compressed_preview_loading = Some(idx);
                            let tx = self.message_sender.clone();
                            let ctx_clone = ctx.clone();
                            let bytes_clone = bytes.clone();

                            std::thread::spawn(move || {
                                if let Ok(img) = image::load_from_memory(&bytes_clone) {
                                    let img = if img.width() > 1280 || img.height() > 1280 {
                                        img.thumbnail(1280, 1280)
                                    } else {
                                        img
                                    };
                                    let rgba = img.to_rgba8();
                                    let (w, h) = rgba.dimensions();
                                    let color_image = egui::ColorImage::from_rgba_unmultiplied(
                                        [w as usize, h as usize],
                                        &rgba.into_raw(),
                                    );
                                    let _ = tx.send(AppMessage::CompressedPreviewLoaded {
                                        index: idx,
                                        color_image,
                                    });
                                    ctx_clone.request_repaint();
                                }
                            });
                        }
                    } else {
                        self.compressed_texture = None;
                        self.compressed_texture_key = Some(idx);
                        self.compressed_preview_loading = None;
                    }
                }
            }
        }
    }
}

impl eframe::App for OmSquishApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [18.0 / 255.0, 12.0 / 255.0, 14.0 / 255.0, 0.82]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame_counter += 1;

        // Check window visibility / occlusion state (Hyprland / Wayland workspace check)
        let is_screenshot = self.screenshot_path.is_some();
        let is_focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        let is_minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));

        if is_screenshot {
            self.is_window_visible = true;
        } else if is_minimized {
            self.is_window_visible = false;
        } else if is_focused {
            self.is_window_visible = true;
        } else if self.last_visibility_check.elapsed() >= std::time::Duration::from_millis(250) {
            self.last_visibility_check = std::time::Instant::now();
            if let Some(visible) = check_hyprland_window_visible(std::process::id()) {
                self.is_window_visible = visible;
            }
        }

        // Drain pending background messages (always pump worker channels regardless of visibility)
        while let Ok(msg) = self.message_receiver.try_recv() {
            match msg {
                AppMessage::FilesLoaded(items) => {
                    self.is_dialog_open = false;
                    let mut existing_set: HashSet<PathBuf> = self
                        .queue
                        .iter()
                        .filter_map(|item| item.file_path.clone())
                        .collect();

                    for mut item in items {
                        if let Some(ref p) = item.file_path {
                            let norm = p.canonicalize().unwrap_or_else(|_| p.clone());
                            if existing_set.contains(p) || existing_set.contains(&norm) {
                                continue;
                            }
                            existing_set.insert(p.clone());
                            existing_set.insert(norm);
                        }
                        item.id = self.queue.len() + 1;
                        self.queue.push(item);
                    }
                    if self.selected_index.is_none() && !self.queue.is_empty() {
                        self.selected_index = Some(0);
                    }
                }
                AppMessage::OutputFolderSelected(folder) => {
                    self.is_dialog_open = false;
                    self.settings.custom_output_folder = Some(folder);
                }
                AppMessage::DialogClosed => {
                    self.is_dialog_open = false;
                }
                AppMessage::OriginalPreviewLoaded {
                    index,
                    bytes,
                    color_image,
                } => {
                    if self.original_preview_loading == Some(index) {
                        self.original_preview_loading = None;
                    }
                    if index < self.queue.len() {
                        self.queue[index].preview_original_bytes = Some(bytes);
                        if self.selected_index == Some(index) {
                            self.original_texture = Some(ctx.load_texture(
                                "preview_orig",
                                color_image,
                                Default::default(),
                            ));
                            self.original_texture_key = Some(index);
                        }
                    }
                }
                AppMessage::CompressedPreviewLoaded {
                    index,
                    color_image,
                } => {
                    if self.compressed_preview_loading == Some(index) {
                        self.compressed_preview_loading = None;
                    }
                    if self.selected_index == Some(index) {
                        self.compressed_texture = Some(ctx.load_texture(
                            "preview_comp",
                            color_image,
                            Default::default(),
                        ));
                        self.compressed_texture_key = Some(index);
                    }
                }
                AppMessage::ItemStarted(index) => {
                    if let Some(item) = self.queue.get_mut(index) {
                        item.status = ProcessStatus::Processing;
                    }
                }
                AppMessage::ItemCompressed { index, result } => {
                    if let Some(item) = self.queue.get_mut(index) {
                        match result {
                            Ok(outcome) => {
                                if let Some(reason) = outcome.skipped_reason {
                                    item.status = ProcessStatus::Skipped(reason);
                                    item.compressed_size = Some(item.original_size);
                                    item.compressed_dimensions = Some(item.original_dimensions);
                                    item.preview_compressed_bytes =
                                        item.preview_original_bytes.clone();
                                } else {
                                    item.compressed_size = Some(outcome.compressed_size);
                                    item.compressed_dimensions = Some(outcome.dimensions);
                                    item.preview_compressed_bytes =
                                        Some(outcome.compressed_bytes);
                                    item.output_path = outcome.output_path;
                                    item.status = ProcessStatus::Finished;
                                }

                                if let Some(ci) = outcome.compressed_preview {
                                    if self.selected_index == Some(index) {
                                        self.compressed_texture = Some(ctx.load_texture(
                                            "preview_comp",
                                            ci,
                                            Default::default(),
                                        ));
                                        self.compressed_texture_key = Some(index);
                                        self.compressed_preview_loading = None;
                                    }
                                } else if self.selected_index == Some(index) {
                                    self.compressed_texture_key = None;
                                    self.compressed_preview_loading = None;
                                }
                            }
                            Err(e) => {
                                item.status = ProcessStatus::Error(e);
                                if self.selected_index == Some(index) {
                                    self.compressed_texture = None;
                                    self.compressed_texture_key = Some(index);
                                    self.compressed_preview_loading = None;
                                }
                            }
                        }

                        let total = self.queue.len();
                        let finished = self
                            .queue
                            .iter()
                            .filter(|q| {
                                matches!(
                                    q.status,
                                    ProcessStatus::Finished
                                        | ProcessStatus::Error(_)
                                        | ProcessStatus::Skipped(_)
                                )
                            })
                            .count();
                        if total > 0 {
                            self.progress = (finished as f32) / (total as f32);
                        }
                    }
                }
                AppMessage::AllCompressionFinished => {
                    self.is_processing = false;
                    self.progress = 1.0;
                    if let Some(idx) = self.selected_index {
                        if self.compressed_texture.is_none()
                            && self
                                .queue
                                .get(idx)
                                .and_then(|q| q.preview_compressed_bytes.as_ref())
                                .is_some()
                        {
                            self.compressed_texture_key = None;
                            self.compressed_preview_loading = None;
                        }
                    }
                }
            }
        }

        // When window is hidden or occluded on another workspace:
        // Skip rendering and GPU swap_buffers / present passes while keeping Wayland socket polling active.
        // Schedule a periodic 250ms timer wakeup (ControlFlow::WaitUntil) so the event loop continually flushes
        // and reads the Wayland socket to answer xdg_wm_base compositor pings without freezing.
        if !self.is_window_visible {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }

        // Active background repaints when visible: ensure 16ms responsiveness during active operations
        if self.is_dialog_open
            || self.is_processing
            || self.original_preview_loading.is_some()
            || self.compressed_preview_loading.is_some()
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        } else {
            // Periodic 250ms timer wakeup even when idle so the main thread continuously polls Wayland
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        // Track hover state for visual drop-zone feedback
        let is_hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        self.is_drag_hovering = is_hovering;

        // Universal Drag-and-Drop and Paste Support (Wayland text/uri-list, X11, raw paths)
        let mut dropped_paths = Vec::new();

        ctx.input(|i| {
            // 1. Process dropped files (winit WindowEvent::DroppedFile, Wayland data offers)
            for file in &i.raw.dropped_files {
                if let Some(ref path) = file.path {
                    let path_str = path.to_string_lossy();
                    if path_str.starts_with("file://") || path_str.contains('\n') || path_str.contains('%') {
                        dropped_paths.extend(parse_uri_list_or_paths(&path_str));
                    } else {
                        dropped_paths.push(path.clone());
                    }
                }
                if let Some(ref bytes) = file.bytes {
                    let content = String::from_utf8_lossy(bytes);
                    dropped_paths.extend(parse_uri_list_or_paths(&content));
                }
                if !file.name.is_empty() {
                    if file.name.starts_with("file://") || file.name.starts_with('/') {
                        dropped_paths.extend(parse_uri_list_or_paths(&file.name));
                    }
                }
            }

            // 2. Clipboard-style text drops or paste events (text/uri-list and text/plain)
            for event in &i.raw.events {
                if let egui::Event::Paste(text) = event {
                    if text.contains("file://") || text.starts_with('/') {
                        dropped_paths.extend(parse_uri_list_or_paths(text));
                    }
                }
            }
        });

        if !dropped_paths.is_empty() {
            self.is_drag_hovering = false;
            self.add_files_async(dropped_paths, ctx);
        }

        // Automated screenshot capture mode
        if let Some(ref path) = self.screenshot_path {
            if self.frame_counter == 120 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
            for event in &ctx.input(|i| i.raw.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = event {
                    let [w, h] = image.size;
                    let mut rgba = Vec::with_capacity(w * h * 4);
                    for pixel in &image.pixels {
                        rgba.extend_from_slice(&pixel.to_array());
                    }
                    let _ = image::save_buffer(
                        path,
                        &rgba,
                        w as u32,
                        h as u32,
                        image::ExtendedColorType::Rgba8,
                    );
                    println!("Screenshot successfully captured to {}", path);
                    std::process::exit(0);
                }
            }
            ctx.request_repaint();
        }

        self.update_textures(ctx);

        // Apply translucent glassy aesthetic matching terminal setup:
        // Window surface: deep charcoal-crimson/obsidian tint rgba(18, 12, 14, 0.82)
        // Inner panels: slightly darker translucent surfaces rgba(14, 10, 12, 0.72) with 1px translucent borders
        let mut visuals = egui::Visuals::dark();
        visuals.override_text_color = Some(Color32::from_rgb(244, 244, 245));
        visuals.panel_fill = Color32::from_rgba_unmultiplied(18, 12, 14, 209); // rgba(18, 12, 14, 0.82)
        visuals.window_fill = Color32::from_rgba_unmultiplied(18, 12, 14, 209);
        visuals.widgets.noninteractive.bg_fill = Color32::from_rgba_unmultiplied(26, 18, 22, 180);
        visuals.widgets.noninteractive.bg_stroke =
            Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 20));
        visuals.widgets.inactive.bg_fill = Color32::from_rgba_unmultiplied(30, 20, 24, 180);
        visuals.widgets.inactive.bg_stroke =
            Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 24));
        visuals.widgets.hovered.bg_fill = Color32::from_rgba_unmultiplied(52, 34, 42, 210);
        visuals.widgets.active.bg_fill = Color32::from_rgb(14, 165, 233); // Cyan/sky accent
        ctx.set_visuals(visuals);

        // 1. Top Bar / Header
        egui::TopBottomPanel::top("header_panel")
            .frame(
                egui::Frame::none()
                    .fill(Color32::from_rgba_unmultiplied(18, 12, 14, 209))
                    .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 24))),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // App icon
                    if self.app_icon_texture.is_none() {
                        let png_bytes = include_bytes!("../assets/icon.png");
                        if let Ok(img) = image::load_from_memory(png_bytes) {
                            let rgba = img.to_rgba8();
                            let (w, h) = rgba.dimensions();
                            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                                [w as usize, h as usize],
                                &rgba.into_raw(),
                            );
                            self.app_icon_texture = Some(ctx.load_texture(
                                "app_icon",
                                color_image,
                                Default::default(),
                            ));
                        }
                    }

                    if let Some(ref icon_tex) = self.app_icon_texture {
                        ui.image((icon_tex.id(), Vec2::new(28.0, 28.0)));
                    } else {
                        ui.label(
                            RichText::new("📷")
                                .font(FontId::proportional(22.0))
                                .color(Color32::from_rgb(14, 165, 233)),
                        );
                    }
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("OmSquish")
                                    .font(FontId::proportional(17.0))
                                    .strong()
                                    .color(Color32::WHITE),
                            );
                            ui.label(
                                RichText::new("Omarchy Image Compressor")
                                    .font(FontId::proportional(15.0))
                                    .color(Color32::from_rgb(161, 161, 170)),
                            );
                            let tag = RichText::new("v2.8.5")
                                .font(FontId::proportional(10.0))
                                .color(Color32::from_rgb(14, 165, 233));
                            ui.colored_label(Color32::from_rgb(14, 165, 233), tag);
                        });
                    });

                    if self.is_dialog_open {
                        ui.separator();
                        ui.label(
                            RichText::new("📂 Selecting files...")
                                .font(FontId::proportional(12.0))
                                .color(Color32::from_rgb(56, 189, 248)),
                        );
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Toggle preview panel
                        let preview_text = if self.show_preview {
                            "👁 Hide Preview"
                        } else {
                            "👁 Show Preview"
                        };
                        if ui.button(preview_text).clicked() {
                            self.show_preview = !self.show_preview;
                        }

                        // Load sample images button
                        if ui.button("⚡ Reset Demos").clicked() {
                            self.queue = generate_sample_images();
                            self.selected_index = Some(0);
                            self.original_texture_key = None;
                            self.compressed_texture_key = None;
                        }

                        // Clear all
                        if ui
                            .button(RichText::new("🧹 Clear").color(Color32::from_rgb(239, 68, 68)))
                            .clicked()
                        {
                            self.queue.clear();
                            self.selected_index = None;
                        }

                        // Remove selected
                        if let Some(sel) = self.selected_index {
                            if ui.button("🗑 Remove").clicked() {
                                if sel < self.queue.len() {
                                    self.queue.remove(sel);
                                    if self.queue.is_empty() {
                                        self.selected_index = None;
                                    } else {
                                        self.selected_index = Some(sel.min(self.queue.len() - 1));
                                    }
                                }
                            }
                        }

                        // Add Folder
                        let folder_btn = ui.add_enabled(
                            !self.is_dialog_open,
                            egui::Button::new("📁 Add Folder..."),
                        );
                        if folder_btn.clicked() {
                            self.spawn_add_folder(ctx);
                        }

                        // Add Files
                        let add_files_btn = ui.add_enabled(
                            !self.is_dialog_open,
                            egui::Button::new(
                                RichText::new("➕ Add Files...")
                                    .strong()
                                    .color(if self.is_dialog_open {
                                        Color32::from_rgb(100, 116, 139)
                                    } else {
                                        Color32::from_rgb(56, 189, 248)
                                    }),
                            ),
                        );
                        if add_files_btn.clicked() {
                            self.spawn_add_files(ctx);
                        }
                    });
                });
            });

        // 2. Bottom Status Bar & Action Button
        egui::TopBottomPanel::bottom("status_bar")
            .frame(
                egui::Frame::none()
                    .fill(Color32::from_rgba_unmultiplied(14, 10, 12, 184))
                    .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 20))),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let total_count = self.queue.len();
                    let total_orig: u64 = self.queue.iter().map(|q| q.original_size).sum();
                    let total_comp: u64 = self
                        .queue
                        .iter()
                        .map(|q| q.compressed_size.unwrap_or(q.original_size))
                        .sum();
                    let total_saved: i64 = total_orig as i64 - total_comp as i64;
                    let saved_pct = if total_orig > 0 {
                        (total_saved as f64 / total_orig as f64 * 100.0) as f32
                    } else {
                        0.0
                    };

                    ui.label(
                        RichText::new(format!("Total: {} files", total_count))
                            .color(Color32::from_rgb(212, 212, 216)),
                    );
                    ui.separator();
                    ui.label(format!("Original: {}", Self::format_bytes(total_orig)));
                    ui.separator();
                    ui.label(format!("Compressed: {}", Self::format_bytes(total_comp)));
                    ui.separator();

                    if total_saved > 0 {
                        ui.label(
                            RichText::new(format!(
                                "Saved: {} ({:.1}%)",
                                Self::format_bytes(total_saved as u64),
                                saved_pct
                            ))
                            .strong()
                            .color(Color32::from_rgb(34, 197, 94)),
                        );
                    } else {
                        ui.label(
                            RichText::new("Saved: 0 B (0.0%)")
                                .color(Color32::from_rgb(161, 161, 170)),
                        );
                    }

                    if self.is_processing {
                        ui.separator();
                        ui.add(
                            egui::ProgressBar::new(self.progress)
                                .desired_width(120.0)
                                .text("Compressing..."),
                        );
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let btn = egui::Button::new(
                            RichText::new(if self.is_processing {
                                "⏳ Compressing..."
                            } else {
                                "🚀 Compress All"
                            })
                            .font(FontId::proportional(15.0))
                            .strong()
                            .color(Color32::WHITE),
                        )
                        .fill(if self.is_processing {
                            Color32::from_rgb(71, 85, 105)
                        } else {
                            Color32::from_rgb(2, 132, 199)
                        })
                        .min_size(Vec2::new(140.0, 32.0));

                        if ui.add_enabled(!self.is_processing, btn).clicked() {
                            self.start_compress_all(ctx);
                        }
                    });
                });
            });

        // 3. Bottom Settings Tabs Panel
        egui::TopBottomPanel::bottom("settings_panel")
            .min_height(145.0)
            .max_height(210.0)
            .frame(
                egui::Frame::none()
                    .fill(Color32::from_rgba_unmultiplied(14, 10, 12, 184))
                    .inner_margin(egui::Margin::symmetric(14.0, 8.0))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 24))),
            )
            .show(ctx, |ui| {
                // Tab selector bar
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.active_tab,
                        SettingsTab::Compression,
                        RichText::new("⚡ Compression").strong(),
                    );
                    ui.selectable_value(
                        &mut self.active_tab,
                        SettingsTab::Resize,
                        RichText::new("📐 Resizing").strong(),
                    );
                    ui.selectable_value(
                        &mut self.active_tab,
                        SettingsTab::Output,
                        RichText::new("📁 Output & Destination").strong(),
                    );
                });
                ui.separator();

                // Tab contents
                match self.active_tab {
                    SettingsTab::Compression => {
                        ui.horizontal(|ui| {
                            // Column 1: Mode & Quality
                            ui.vertical(|ui| {
                                ui.label(RichText::new("Compression Mode").strong());
                                ui.horizontal(|ui| {
                                    ui.radio_value(
                                        &mut self.settings.mode,
                                        CompressionMode::Quality,
                                        "Quality Mode",
                                    );
                                    ui.radio_value(
                                        &mut self.settings.mode,
                                        CompressionMode::TargetSize,
                                        "Target Max Size",
                                    );
                                });

                                match self.settings.mode {
                                    CompressionMode::Quality => {
                                        ui.horizontal(|ui| {
                                            ui.label("JPEG Quality:");
                                            ui.add(
                                                egui::Slider::new(
                                                    &mut self.settings.jpeg_quality,
                                                    1..=100,
                                                )
                                                .suffix("%"),
                                            );
                                        });
                                        ui.horizontal(|ui| {
                                            ui.label("PNG Quality:");
                                            ui.add(
                                                egui::Slider::new(
                                                    &mut self.settings.png_quality,
                                                    1..=100,
                                                )
                                                .suffix("%"),
                                            );
                                        });
                                        ui.horizontal(|ui| {
                                            ui.label("WebP Quality:");
                                            ui.add(
                                                egui::Slider::new(
                                                    &mut self.settings.webp_quality,
                                                    1..=100,
                                                )
                                                .suffix("%"),
                                            );
                                        });
                                    }
                                    CompressionMode::TargetSize => {
                                        ui.horizontal(|ui| {
                                            ui.label("Target Size (KB):");
                                            ui.add(
                                                egui::DragValue::new(
                                                    &mut self.settings.target_size_kb,
                                                )
                                                .speed(10)
                                                .range(10..=100000),
                                            );
                                            ui.label("KB");
                                        });
                                    }
                                }
                            });

                            ui.separator();

                            // Column 2: Advanced Format Flags
                            ui.vertical(|ui| {
                                ui.label(RichText::new("Format Details").strong());
                                ui.checkbox(
                                    &mut self.settings.jpeg_progressive,
                                    "JPEG Progressive Encoding",
                                );
                                ui.horizontal(|ui| {
                                    ui.label("PNG Optimization Level:");
                                    ui.add(egui::Slider::new(
                                        &mut self.settings.png_optimization_level,
                                        0..=6,
                                    ));
                                });
                                ui.checkbox(
                                    &mut self.settings.webp_lossless,
                                    "WebP Lossless Mode",
                                );
                                ui.checkbox(
                                    &mut self.settings.keep_metadata,
                                    "Keep EXIF Metadata",
                                );
                            });
                        });
                    }
                    SettingsTab::Resize => {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.checkbox(&mut self.settings.resize_enabled, "Enable Resizing");
                                ui.add_enabled_ui(self.settings.resize_enabled, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label("Fit to:");
                                        egui::ComboBox::from_id_salt("resize_mode_combo")
                                            .selected_text(match self.settings.resize_mode {
                                                ResizeMode::Dimensions => "Dimensions (W x H)",
                                                ResizeMode::Percentage => "Percentage (%)",
                                                ResizeMode::Width => "Width",
                                                ResizeMode::Height => "Height",
                                            })
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut self.settings.resize_mode,
                                                    ResizeMode::Dimensions,
                                                    "Dimensions (W x H)",
                                                );
                                                ui.selectable_value(
                                                    &mut self.settings.resize_mode,
                                                    ResizeMode::Percentage,
                                                    "Percentage (%)",
                                                );
                                                ui.selectable_value(
                                                    &mut self.settings.resize_mode,
                                                    ResizeMode::Width,
                                                    "Width",
                                                );
                                                ui.selectable_value(
                                                    &mut self.settings.resize_mode,
                                                    ResizeMode::Height,
                                                    "Height",
                                                );
                                            });
                                    });

                                    match self.settings.resize_mode {
                                        ResizeMode::Dimensions => {
                                            ui.horizontal(|ui| {
                                                ui.label("Width:");
                                                ui.add(
                                                    egui::DragValue::new(
                                                        &mut self.settings.resize_width,
                                                    )
                                                    .speed(10)
                                                    .range(1..=16384),
                                                );
                                                ui.label("px  Height:");
                                                ui.add(
                                                    egui::DragValue::new(
                                                        &mut self.settings.resize_height,
                                                    )
                                                    .speed(10)
                                                    .range(1..=16384),
                                                );
                                                ui.label("px");
                                            });
                                        }
                                        ResizeMode::Percentage => {
                                            ui.horizontal(|ui| {
                                                ui.label("Scale:");
                                                ui.add(
                                                    egui::Slider::new(
                                                        &mut self.settings.resize_percentage,
                                                        1..=200,
                                                    )
                                                    .suffix("%"),
                                                );
                                            });
                                        }
                                        ResizeMode::Width => {
                                            ui.horizontal(|ui| {
                                                ui.label("Width:");
                                                ui.add(
                                                    egui::DragValue::new(
                                                        &mut self.settings.resize_width,
                                                    )
                                                    .speed(10)
                                                    .range(1..=16384),
                                                );
                                                ui.label("px");
                                            });
                                        }
                                        ResizeMode::Height => {
                                            ui.horizontal(|ui| {
                                                ui.label("Height:");
                                                ui.add(
                                                    egui::DragValue::new(
                                                        &mut self.settings.resize_height,
                                                    )
                                                    .speed(10)
                                                    .range(1..=16384),
                                                );
                                                ui.label("px");
                                            });
                                        }
                                    }

                                    ui.checkbox(
                                        &mut self.settings.keep_aspect_ratio,
                                        "Keep Aspect Ratio",
                                    );
                                    ui.checkbox(
                                        &mut self.settings.do_not_enlarge,
                                        "Do Not Enlarge (if source is smaller)",
                                    );
                                });
                            });
                        });
                    }
                    SettingsTab::Output => {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.checkbox(
                                    &mut self.settings.same_folder_as_input,
                                    "Save in same directory as input file",
                                );

                                if !self.settings.same_folder_as_input {
                                    ui.horizontal(|ui| {
                                        ui.label("Destination folder:");
                                        let folder_str = self
                                            .settings
                                            .custom_output_folder
                                            .as_ref()
                                            .map(|p| p.to_string_lossy().to_string())
                                            .unwrap_or_else(|| "Select folder...".to_string());
                                        ui.monospace(folder_str);
                                        let browse_btn = ui.add_enabled(
                                            !self.is_dialog_open,
                                            egui::Button::new("Browse..."),
                                        );
                                        if browse_btn.clicked() {
                                            self.spawn_browse_output_folder(ctx);
                                        }
                                    });
                                }

                                ui.horizontal(|ui| {
                                    ui.label("Filename suffix:");
                                    ui.text_edit_singleline(&mut self.settings.output_suffix);
                                    ui.label("(e.g. photo_compressed.jpg)");
                                });
                            });

                            ui.separator();

                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label("Convert format:");
                                    egui::ComboBox::from_id_salt("output_format_combo")
                                        .selected_text(match self.settings.output_format {
                                            OutputFormat::KeepOriginal => "Keep Original Format",
                                            OutputFormat::Jpeg => "Convert to JPEG",
                                            OutputFormat::Png => "Convert to PNG",
                                            OutputFormat::Webp => "Convert to WebP",
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut self.settings.output_format,
                                                OutputFormat::KeepOriginal,
                                                "Keep Original Format",
                                            );
                                            ui.selectable_value(
                                                &mut self.settings.output_format,
                                                OutputFormat::Jpeg,
                                                "Convert to JPEG",
                                            );
                                            ui.selectable_value(
                                                &mut self.settings.output_format,
                                                OutputFormat::Png,
                                                "Convert to PNG",
                                            );
                                            ui.selectable_value(
                                                &mut self.settings.output_format,
                                                OutputFormat::Webp,
                                                "Convert to WebP",
                                            );
                                        });
                                });

                                ui.checkbox(
                                    &mut self.settings.skip_if_bigger,
                                    "Skip if output is larger than original",
                                );
                                ui.checkbox(
                                    &mut self.settings.keep_dates,
                                    "Preserve file modification timestamp",
                                );
                            });
                        });
                    }
                }
            });

        // 4. Right Side Preview Panel (if enabled)
        if self.show_preview {
            egui::SidePanel::right("preview_panel")
                .min_width(320.0)
                .max_width(480.0)
                .frame(
                    egui::Frame::none()
                        .fill(Color32::from_rgba_unmultiplied(14, 10, 12, 184))
                        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
                        .stroke(Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 24))),
                )
                .show(ctx, |ui| {
                    ui.label(
                        RichText::new("Image Comparison Preview")
                            .font(FontId::proportional(15.0))
                            .strong()
                            .color(Color32::WHITE),
                    );
                    ui.separator();

                    if let Some(idx) = self.selected_index {
                        if idx < self.queue.len() {
                            let item = &self.queue[idx];

                            ui.label(
                                RichText::new(&item.filename)
                                    .strong()
                                    .color(Color32::from_rgb(56, 189, 248)),
                            );

                            ui.add_space(4.0);

                            // Original Preview Card (100% opaque solid backing)
                            egui::Frame::none()
                                .fill(Color32::from_rgb(14, 11, 13))
                                .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                                .stroke(Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 24)))
                                .rounding(6.0)
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("ORIGINAL").strong());
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.label(format!(
                                                    "{} | {}x{}",
                                                    Self::format_bytes(item.original_size),
                                                    item.original_dimensions.0,
                                                    item.original_dimensions.1
                                                ));
                                            },
                                        );
                                    });

                                    if let Some(ref tex) = self.original_texture {
                                        let aspect = tex.size()[0] as f32 / tex.size()[1].max(1) as f32;
                                        let width = ui.available_width().min(380.0);
                                        let height = (width / aspect).min(160.0);
                                        ui.image((tex.id(), Vec2::new(width, height)));
                                    } else if self.original_preview_loading == Some(idx) {
                                        ui.colored_label(
                                            Color32::from_rgb(161, 161, 170),
                                            "⏳ Loading preview...",
                                        );
                                    } else {
                                        ui.label("No preview available");
                                    }
                                });

                            ui.add_space(6.0);

                            // Compressed Preview Card (100% opaque solid backing)
                            egui::Frame::none()
                                .fill(Color32::from_rgb(14, 11, 13))
                                .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                                .stroke(Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 24)))
                                .rounding(6.0)
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new("COMPRESSED")
                                                .strong()
                                                .color(Color32::from_rgb(34, 197, 94)),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if let Some(comp_sz) = item.compressed_size {
                                                    let (cw, ch) = item
                                                        .compressed_dimensions
                                                        .unwrap_or(item.original_dimensions);
                                                    ui.label(
                                                        RichText::new(format!(
                                                            "{} | {}x{} ({:.1}%)",
                                                            Self::format_bytes(comp_sz),
                                                            cw,
                                                            ch,
                                                            item.savings_percentage()
                                                        ))
                                                        .color(Color32::from_rgb(34, 197, 94)),
                                                    );
                                                } else {
                                                    ui.label("Pending compression");
                                                }
                                            },
                                        );
                                    });

                                    if let Some(ref tex) = self.compressed_texture {
                                        let aspect = tex.size()[0] as f32 / tex.size()[1].max(1) as f32;
                                        let width = ui.available_width().min(380.0);
                                        let height = (width / aspect).min(160.0);
                                        ui.image((tex.id(), Vec2::new(width, height)));
                                    } else if self.compressed_preview_loading == Some(idx) {
                                        ui.colored_label(
                                            Color32::from_rgb(161, 161, 170),
                                            "⏳ Generating compressed preview...",
                                        );
                                    } else {
                                        ui.colored_label(
                                            Color32::from_rgb(161, 161, 170),
                                            "Click 'Compress All' to generate preview",
                                        );
                                    }
                                });
                        }
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label("Select an image from the queue to view preview");
                        });
                    }
                });
        }

        // 5. Central Workspace: Image Queue Table
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(Color32::from_rgba_unmultiplied(14, 10, 12, 184))
                    .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                    .stroke(if self.is_drag_hovering {
                        Stroke::new(2.0_f32, Color32::from_rgb(251, 113, 133))
                    } else {
                        Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 22))
                    }),
            )
            .show(ctx, |ui| {
                if self.queue.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new("📥 Drag & drop images or click '+ Add Files...'")
                                    .font(FontId::proportional(16.0))
                                    .color(Color32::from_rgb(161, 161, 170)),
                            );
                            ui.add_space(8.0);
                            if ui.button("⚡ Load Demo Images").clicked() {
                                self.queue = generate_sample_images();
                                self.selected_index = Some(0);
                            }
                        });
                    });
                } else {
                    // Queue Table
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        // Header row
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("#").strong().color(Color32::from_rgb(161, 161, 170)));
                            ui.add_space(8.0);
                            ui.label(RichText::new("Filename").strong().color(Color32::from_rgb(161, 161, 170)));
                            ui.add_space(140.0);
                            ui.label(RichText::new("Format").strong().color(Color32::from_rgb(161, 161, 170)));
                            ui.add_space(20.0);
                            ui.label(RichText::new("Original").strong().color(Color32::from_rgb(161, 161, 170)));
                            ui.add_space(20.0);
                            ui.label(RichText::new("Compressed").strong().color(Color32::from_rgb(161, 161, 170)));
                            ui.add_space(20.0);
                            ui.label(RichText::new("Saved").strong().color(Color32::from_rgb(161, 161, 170)));
                            ui.add_space(20.0);
                            ui.label(RichText::new("Resolution").strong().color(Color32::from_rgb(161, 161, 170)));
                            ui.add_space(20.0);
                            ui.label(RichText::new("Status").strong().color(Color32::from_rgb(161, 161, 170)));
                        });
                        ui.separator();

                        for idx in 0..self.queue.len() {
                            let is_selected = self.selected_index == Some(idx);
                            let item = &self.queue[idx];

                            let bg_color = if is_selected {
                                Color32::from_rgba_unmultiplied(48, 28, 36, 220) // Selected row dark charcoal-crimson highlight
                            } else if idx % 2 == 0 {
                                Color32::from_rgba_unmultiplied(22, 15, 18, 140)
                            } else {
                                Color32::from_rgba_unmultiplied(16, 11, 14, 90)
                            };

                            let frame = egui::Frame::none()
                                .fill(bg_color)
                                .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                                .stroke(if is_selected {
                                    Stroke::new(1.0_f32, Color32::from_rgb(14, 165, 233))
                                } else {
                                    Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 10))
                                })
                                .rounding(4.0);

                            let response = frame.show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    // Index
                                    ui.label(format!("{:02}", idx + 1));
                                    ui.add_space(8.0);

                                    // Filename
                                    let fname = if item.filename.len() > 24 {
                                        format!("{}...", &item.filename[..21])
                                    } else {
                                        item.filename.clone()
                                    };
                                    ui.label(RichText::new(fname).strong());
                                    ui.add_space(30.0);

                                    // Format
                                    ui.label(item.format.display_name());
                                    ui.add_space(30.0);

                                    // Original Size
                                    ui.label(Self::format_bytes(item.original_size));
                                    ui.add_space(30.0);

                                    // Compressed Size
                                    if let Some(comp) = item.compressed_size {
                                        ui.label(Self::format_bytes(comp));
                                    } else {
                                        ui.label("-");
                                    }
                                    ui.add_space(35.0);

                                    // Savings
                                    if item.compressed_size.is_some() {
                                        let pct = item.savings_percentage();
                                        if pct > 0.0 {
                                            ui.label(
                                                RichText::new(format!("-{:.1}%", pct))
                                                    .strong()
                                                    .color(Color32::from_rgb(34, 197, 94)),
                                            );
                                        } else {
                                            ui.label(format!("{:.1}%", pct));
                                        }
                                    } else {
                                        ui.label("-");
                                    }
                                    ui.add_space(35.0);

                                    // Resolution
                                    ui.label(format!(
                                        "{}x{}",
                                        item.original_dimensions.0, item.original_dimensions.1
                                    ));
                                    ui.add_space(30.0);

                                    // Status
                                    match &item.status {
                                        ProcessStatus::Ready => {
                                            ui.colored_label(Color32::from_rgb(161, 161, 170), "Ready");
                                        }
                                        ProcessStatus::Processing => {
                                            ui.colored_label(Color32::from_rgb(56, 189, 248), "Compressing...");
                                        }
                                        ProcessStatus::Finished => {
                                            ui.colored_label(Color32::from_rgb(34, 197, 94), "✓ Finished");
                                        }
                                        ProcessStatus::Skipped(msg) => {
                                            ui.colored_label(Color32::from_rgb(234, 179, 8), format!("⚠ {}", msg));
                                        }
                                        ProcessStatus::Error(err) => {
                                            ui.colored_label(Color32::from_rgb(239, 68, 68), format!("❌ {}", err));
                                        }
                                    }
                                });
                            });

                            if response.response.interact(egui::Sense::click()).clicked() {
                                self.selected_index = Some(idx);
                                self.original_texture_key = None;
                                self.compressed_texture_key = None;
                            }
                        }
                    });
                }

                // Visual drop-zone feedback overlay when dragging files over the window
                if self.is_drag_hovering {
                    let panel_rect = ui.max_rect();
                    let painter = ui.painter();

                    // Translucent dark crimson backdrop over entire queue area
                    painter.rect_filled(
                        panel_rect,
                        6.0,
                        Color32::from_rgba_unmultiplied(22, 13, 16, 235),
                    );

                    // Subtle dashed coral/accent border
                    draw_dashed_rect(
                        painter,
                        panel_rect.shrink(6.0),
                        Color32::from_rgb(251, 113, 133),
                        10.0,
                        6.0,
                        2.0,
                    );

                    let center = panel_rect.center();
                    painter.text(
                        center - egui::vec2(0.0, 22.0),
                        egui::Align2::CENTER_CENTER,
                        "📥 Drop images or folders to add",
                        FontId::proportional(22.0),
                        Color32::from_rgb(251, 113, 133),
                    );
                    painter.text(
                        center + egui::vec2(0.0, 18.0),
                        egui::Align2::CENTER_CENTER,
                        "Supports JPG, PNG, and WebP (recursively scanned & deduplicated)",
                        FontId::proportional(14.0),
                        Color32::from_rgb(212, 212, 216),
                    );
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_parse_uri_list_standard() {
        let uri_data = "file:///home/user/Pictures/photo.jpg\r\nfile:///home/user/Pictures/image.png\n";
        let paths = parse_uri_list_or_paths(uri_data);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], PathBuf::from("/home/user/Pictures/photo.jpg"));
        assert_eq!(paths[1], PathBuf::from("/home/user/Pictures/image.png"));
    }

    #[test]
    fn test_parse_uri_list_with_crlf_and_comments() {
        let uri_data = "# This is a comment from file manager\r\nfile:///home/user/Wallpapers/sunset.webp\r\n\r\n# Another comment\nfile:///home/user/Pictures/banner.jpeg\n";
        let paths = parse_uri_list_or_paths(uri_data);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], PathBuf::from("/home/user/Wallpapers/sunset.webp"));
        assert_eq!(paths[1], PathBuf::from("/home/user/Pictures/banner.jpeg"));
    }

    #[test]
    fn test_parse_uri_list_percent_encoded_spaces_and_symbols() {
        let uri_data = "file:///home/user/My%20Photos%20%28Vacation%29/summer%20trip%20%231.png\r\n";
        let paths = parse_uri_list_or_paths(uri_data);
        assert_eq!(paths.len(), 1);
        assert_eq!(
            paths[0],
            PathBuf::from("/home/user/My Photos (Vacation)/summer trip #1.png")
        );
    }

    #[test]
    fn test_parse_uri_list_utf8() {
        let uri_data = "file:///home/user/Pictures/caf%C3%A9_au_lait.jpg\n";
        let paths = parse_uri_list_or_paths(uri_data);
        assert_eq!(paths.len(), 1);
        assert_eq!(
            paths[0],
            PathBuf::from("/home/user/Pictures/café_au_lait.jpg")
        );
    }

    #[test]
    fn test_parse_uri_list_localhost_prefix() {
        let uri_data = "file://localhost/home/user/Downloads/archive.png\r\n";
        let paths = parse_uri_list_or_paths(uri_data);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from("/home/user/Downloads/archive.png"));
    }

    #[test]
    fn test_parse_raw_paths_and_quotes() {
        let raw_data = "\"/home/user/Pictures/quoted path.png\"\n/home/user/Pictures/normal.webp\n";
        let paths = parse_uri_list_or_paths(raw_data);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], PathBuf::from("/home/user/Pictures/quoted path.png"));
        assert_eq!(paths[1], PathBuf::from("/home/user/Pictures/normal.webp"));
    }

    #[test]
    fn test_is_supported_image() {
        assert!(is_supported_image(&PathBuf::from("photo.jpg")));
        assert!(is_supported_image(&PathBuf::from("photo.JPEG")));
        assert!(is_supported_image(&PathBuf::from("/path/to/icon.png")));
        assert!(is_supported_image(&PathBuf::from("graphic.webp")));
        assert!(is_supported_image(&PathBuf::from("GRAPHIC.WEBP")));

        assert!(!is_supported_image(&PathBuf::from("notes.txt")));
        assert!(!is_supported_image(&PathBuf::from("binary.exe")));
        assert!(!is_supported_image(&PathBuf::from("document.pdf")));
        assert!(!is_supported_image(&PathBuf::from("no_extension")));
    }

    #[test]
    fn test_recursive_folder_and_deduplication() {
        let temp_dir = std::env::temp_dir().join("omsquish_dnd_test");
        let sub_dir = temp_dir.join("subfolder");
        let _ = std::fs::create_dir_all(&sub_dir);

        let img1 = temp_dir.join("test1.jpg");
        let img2 = temp_dir.join("test2.png");
        let img3 = sub_dir.join("nested.webp");
        let text_file = sub_dir.join("readme.txt");

        let _ = std::fs::write(&img1, b"fake_jpeg");
        let _ = std::fs::write(&img2, b"fake_png");
        let _ = std::fs::write(&img3, b"fake_webp");
        let _ = std::fs::write(&text_file, b"skip this");

        // Simulate background folder walker & deduplication
        let mut seen_paths = std::collections::HashSet::new();
        let mut collected = Vec::new();

        let input_paths = vec![temp_dir.clone(), img1.clone()]; // note: img1 is passed both as folder member and direct file

        for path in input_paths {
            if path.is_dir() {
                for entry in walkdir::WalkDir::new(&path)
                    .follow_links(true)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    if entry.file_type().is_file() {
                        let p = entry.path().to_path_buf();
                        if is_supported_image(&p) {
                            let norm = p.canonicalize().unwrap_or_else(|_| p.clone());
                            if !seen_paths.contains(&p) && !seen_paths.contains(&norm) {
                                seen_paths.insert(p.clone());
                                seen_paths.insert(norm);
                                collected.push(p);
                            }
                        }
                    }
                }
            } else if path.is_file() {
                if is_supported_image(&path) {
                    let norm = path.canonicalize().unwrap_or_else(|_| path.clone());
                    if !seen_paths.contains(&path) && !seen_paths.contains(&norm) {
                        seen_paths.insert(path.clone());
                        seen_paths.insert(norm);
                        collected.push(path);
                    }
                }
            }
        }

        // Clean up temp files
        let _ = std::fs::remove_dir_all(&temp_dir);

        assert_eq!(collected.len(), 3);
        let names: Vec<String> = collected
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"test1.jpg".to_string()));
        assert!(names.contains(&"test2.png".to_string()));
        assert!(names.contains(&"nested.webp".to_string()));
        assert!(!names.contains(&"readme.txt".to_string()));
    }
}
