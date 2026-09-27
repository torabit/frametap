//! 実機とデバイスを使わずに画面を出す。
//!
//! README に載せる画像と、表示の手直しを Windows へ渡す前に見るために使う。合成した
//! 入力を [`History`] に積み、実際に配るものと同じ [`frametap::ui::show`] を呼ぶ。
//!
//! ```
//! cargo run --example demo
//! ```

use std::sync::{Arc, Mutex};

use eframe::egui::ViewportBuilder;
use frametap::history::{History, DEFAULT_RETAIN_US, DEFAULT_TRIAL_GAP_US};
use frametap::report_decode::{Buttons, Direction};
use frametap::timeline::{EventKind, InputEvent, Target};
use frametap::ui::{self, Settings, Status};

/// 60fps の 1F。
const FRAME_US: u64 = 16_667;

fn press(target: Target, at_us: u64) -> InputEvent {
    InputEvent {
        kind: EventKind::Press,
        target,
        at_us,
        gap_before: false,
    }
}

fn release(target: Target, at_us: u64) -> InputEvent {
    InputEvent {
        kind: EventKind::Release,
        ..press(target, at_us)
    }
}

/// ローリングの直後に攻撃を入れた形を 3 回。1 回ごとに 1F ずつ遅らせる。
///
/// この道具が読みたいのは、その 1F の差が数字に出るかどうかになる。
fn history() -> History {
    let mut history = History::new(DEFAULT_RETAIN_US, DEFAULT_TRIAL_GAP_US);
    let mut at_us = 0;

    for delay_frames in 0..3u64 {
        // 前方向を入れてローリング。
        history.push(&[press(Target::Stick(Direction::N), at_us)], at_us);
        history.push(
            &[press(Target::Button(Buttons::CIRCLE), at_us + FRAME_US)],
            at_us + FRAME_US,
        );
        history.push(
            &[release(
                Target::Button(Buttons::CIRCLE),
                at_us + 3 * FRAME_US,
            )],
            at_us + 3 * FRAME_US,
        );

        // 攻撃。試行ごとに 1F ずつ遅れる。
        let attack_us = at_us + (18 + delay_frames) * FRAME_US;
        history.push(&[press(Target::Button(Buttons::R1), attack_us)], attack_us);
        history.push(
            &[release(
                Target::Button(Buttons::R1),
                attack_us + 2 * FRAME_US,
            )],
            attack_us + 2 * FRAME_US,
        );
        history.push(
            &[release(
                Target::Stick(Direction::N),
                attack_us + 4 * FRAME_US,
            )],
            attack_us + 4 * FRAME_US,
        );

        at_us = attack_us + 4 * FRAME_US + 4 * DEFAULT_TRIAL_GAP_US;
    }

    history.push(&[], at_us);
    history
}

struct Demo {
    history: Arc<Mutex<History>>,
    status: Status,
    settings: Settings,
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        let trials = {
            let history = self.history.lock().expect("毒されていない");
            ui::recent_trials(&history, &self.settings)
        };

        ui::show(ui, &trials, &self.status, &self.settings, &[]);
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("frametap")
            .with_inner_size([440.0, 620.0]),
        ..Default::default()
    };

    eframe::run_native(
        "frametap demo",
        options,
        Box::new(|_cc| {
            Ok(Box::new(Demo {
                history: Arc::new(Mutex::new(history())),
                status: Status {
                    scale_us_per_tick: Some(0.3274),
                    ..Status::connected("DualSense".to_owned())
                },
                settings: Settings::default(),
            }))
        }),
    )
}
