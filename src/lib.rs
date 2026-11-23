pub mod compressor;
pub mod sample_data;
pub mod types;
pub mod ui;

use ui::OmSquishApp;

pub fn load_icon() -> Option<egui::IconData> {
    let png_bytes = include_bytes!("../assets/icon.png");
    if let Ok(img) = image::load_from_memory(png_bytes) {
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        Some(egui::IconData {
            rgba: rgba.into_raw(),
            width,
            height,
        })
    } else {
        None
    }
}

pub fn run_app() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut screenshot_path: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        if args[i] == "--screenshot" && i + 1 < args.len() {
            screenshot_path = Some(args[i + 1].clone());
            i += 2;
        } else {
            i += 1;
        }
    }

    let is_screenshot_mode = screenshot_path.is_some();

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("OmSquish")
        .with_app_id("omsquish")
        .with_inner_size([1140.0, 720.0])
        .with_min_inner_size([880.0, 560.0])
        .with_transparent(true);

    if let Some(icon) = load_icon() {
        viewport = viewport.with_icon(icon);
    }

    if is_screenshot_mode {
        viewport = viewport.with_active(false);
    }

    let options = eframe::NativeOptions {
        viewport,
        vsync: false,
        ..Default::default()
    };

    eframe::run_native(
        "OmSquish",
        options,
        Box::new(move |_cc| Ok(Box::new(OmSquishApp::new(screenshot_path)))),
    )
}
