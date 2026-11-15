mod audio;
mod cli;
mod session;
mod ui;

fn main() -> eframe::Result {
    if let Some(code) = cli::run() {
        std::process::exit(code);
    }
    let options = eframe::NativeOptions {
        // On Wayland, waiting for vsync can block buffer swaps indefinitely when
        // a workspace is hidden, preventing compositor pings from being answered.
        // The UI already paces its redraws with request_repaint_after instead.
        vsync: false,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("OmaScribe")
            .with_app_id("omascribe")
            .with_inner_size([1180., 760.])
            .with_min_inner_size([850., 600.]),
        ..Default::default()
    };
    eframe::run_native(
        "OmaScribe",
        options,
        Box::new(|cc| Ok(Box::new(ui::App::new(cc)))),
    )
}
