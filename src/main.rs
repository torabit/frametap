//! ウィンドウを開いて入力スレッドを起動する。
//!
//! サブモニタに置いてゲームと並走させるため、always-on-top で開く。

use eframe::egui::ViewportBuilder;
use frametap::{app::FrametapApp, ui};

/// 初期のウィンドウサイズ。名前の列と F の 2 列が入り、試行が数件見える幅と高さ。
const WINDOW_SIZE: [f32; 2] = [420.0, 640.0];

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("frametap")
            .with_always_on_top()
            .with_inner_size(WINDOW_SIZE),
        ..Default::default()
    };

    eframe::run_native(
        "frametap",
        options,
        Box::new(|_cc| Ok(Box::new(FrametapApp::new(ui::Settings::default())))),
    )
}
