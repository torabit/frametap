//! ウィンドウを開いて入力スレッドを起動する。
//!
//! サブモニタに置いてゲームと並走させるため、always-on-top で開く。
//!
//! release build では console を出さない。常時置いておく道具なので、起動のたびに
//! 黒い窓が並ぶと邪魔になる。debug build では残す。開発中は標準出力を読む。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::ViewportBuilder;
use frametap::{app::FrametapApp, config, panic_report};

/// 初期のウィンドウサイズ。名前の列と F の 2 列が入り、試行が数件見える幅と高さ。
const WINDOW_SIZE: [f32; 2] = [420.0, 640.0];

fn main() -> eframe::Result<()> {
    // console が無い release build では、既定の hook が書いた内容がどこにも出ない。
    // 設定の読み込みより先に差し込む。読み込みの中で落ちても報告を残すため。
    panic_report::install();

    let loaded = config::load();

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
        Box::new(move |_cc| Ok(Box::new(FrametapApp::new(loaded)))),
    )
}
