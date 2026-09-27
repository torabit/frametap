//! スレッドの起動と配線。
//!
//! スレッドは 2 本ある。入力スレッドは [`crate::hid_source`] のブロッキング read から
//! [`History`] までを回し、UI スレッドは eframe のループで [`History`] を読む。
//! 250Hz の書き込みと 60Hz の読み出しなので [`Mutex`] で足りる。ロックを持つのは
//! push と snapshot のコピーの間だけにする。

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui;

use crate::config::{Config, Loaded};
use crate::hid_source;
use crate::history::{History, Trial, DEFAULT_RETAIN_US};
use crate::report_decode::decode;
use crate::timeline::{EventKind, InputEvent, Target, Timeline};
use crate::ui;

/// 方向を切り替える角度の閾値。セクタの半分にすると、隣のセクタに入った時点で切り替わる。
const STICK_HYSTERESIS_DEG: f32 = 22.5;

/// 抜き差しの再試行間隔。
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);

/// UI を描き直す間隔。押しっぱなしの長さは入力が無くても伸びるので、待たずに描き直す。
const REPAINT_INTERVAL: Duration = Duration::from_millis(16);

/// 設定の ms を µs に直す。桁溢れは上限で止める。設定ファイルの値を検算していないため。
fn trial_gap_us(config: &Config) -> u64 {
    config.trial_gap_ms.saturating_mul(1_000)
}

/// 設定から表示の設定を取り出す。[`ui`] に [`Config`] を知らせないため、変換をここに置く。
fn settings_from(config: &Config) -> ui::Settings {
    ui::Settings {
        fps: config.fps,
        trials_shown: config.trials_shown,
    }
}

/// 入力スレッドと UI スレッドが共有する状態。
#[derive(Debug)]
pub struct Shared {
    history: Mutex<History>,
    status: Mutex<ui::Status>,
}

impl Shared {
    pub fn new(config: &Config) -> Self {
        Self {
            history: Mutex::new(History::new(DEFAULT_RETAIN_US, trial_gap_us(config))),
            status: Mutex::new(ui::Status::default()),
        }
    }

    /// 表示に使う試行を複製する。何件読むかの判断は [`ui::recent_trials`] が持つ。
    /// ロックはこの複製の間だけで、描画の間は持たない。
    pub fn snapshot(&self, settings: &ui::Settings) -> Vec<Trial> {
        ui::recent_trials(&self.lock_history(), settings)
    }

    pub fn status(&self) -> ui::Status {
        self.lock_status().clone()
    }

    fn push(&self, events: &[InputEvent], now_us: u64) {
        self.lock_history().push(events, now_us);
    }

    fn now_us(&self) -> u64 {
        self.lock_history().now_us()
    }

    /// 毒された Mutex でも中身を取り出して続ける。入力スレッドが panic したときに
    /// UI まで落とすと、直前まで溜めた履歴を読めなくなる。[`Self::lock_status`] も同じ。
    fn lock_history(&self) -> std::sync::MutexGuard<'_, History> {
        self.history.lock().unwrap_or_else(|err| err.into_inner())
    }

    fn lock_status(&self) -> std::sync::MutexGuard<'_, ui::Status> {
        self.status.lock().unwrap_or_else(|err| err.into_inner())
    }
}

impl Default for Shared {
    fn default() -> Self {
        Self::new(&Config::default())
    }
}

/// eframe に渡すアプリ。生成した時点で入力スレッドが走り出す。
pub struct FrametapApp {
    shared: Arc<Shared>,
    settings: ui::Settings,
    /// 設定ファイルを読んだときの警告。画面に出したままにする。
    notices: Vec<String>,
}

impl FrametapApp {
    pub fn new(loaded: Loaded) -> Self {
        let Loaded { config, warnings } = loaded;
        let shared = Arc::new(Shared::new(&config));
        let input = Arc::clone(&shared);
        thread::spawn(move || run_input(&input, &config));

        Self {
            shared,
            settings: settings_from(&config),
            notices: warnings,
        }
    }
}

impl eframe::App for FrametapApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let trials = self.shared.snapshot(&self.settings);
        let status = self.shared.status();

        // egui の慣習に合わせて引数を ui と呼ぶため、モジュールは絶対パスで書く。
        crate::ui::show(ui, &trials, &status, &self.settings, &self.notices);

        // 押しっぱなしの持続 F は入力が無くても伸びる。イベント待ちで止めない。
        ui.ctx().request_repaint_after(REPAINT_INTERVAL);
    }
}

/// 接続を張り直しながらレポートを読み続ける。戻らない。
///
/// 履歴は接続をまたいで残す。抜き差しの前に読んでいた試行が消えると、
/// 何が起きたかを確かめる手段がなくなる。
fn run_input(shared: &Shared, config: &Config) {
    loop {
        let reason = match hid_source::open() {
            Ok(connection) => {
                *shared.lock_status() = ui::Status::connected(connection.name().to_owned());
                pump(shared, &connection, config)
            }
            Err(reason) => reason,
        };

        // 代入の文で guard を捨てる。次の sleep の間ロックを持つと、その 1 秒は UI が状態を読めない。
        *shared.lock_status() = ui::Status::disconnected(reason);

        thread::sleep(RECONNECT_INTERVAL);
    }
}

/// 1 接続分のレポートを読み続ける。戻り値は読みが止まった理由。
fn pump(shared: &Shared, connection: &hid_source::Connection, config: &Config) -> String {
    let device = connection.device();
    let mut timeline = Timeline::new(device, config.stick_deadzone, STICK_HYSTERESIS_DEG);

    // [`Timeline`] が出す時刻は接続ごとに 0 から始まる。履歴の時刻は戻せないので、
    // 前の接続の末尾から試行の区切り以上空けた位置に載せ直す。空けないと抜き差しの
    // 前後が 1 試行に繋がる。
    let base_us = shared.now_us() + trial_gap_us(config);
    let mut held: Vec<Target> = Vec::new();

    loop {
        let report = match connection.read() {
            Ok(report) => report,
            Err(reason) => {
                // 抜かれた時点で押されていたものは、離しが届かない。ここで離しておかないと
                // 進行中の試行が押しっぱなしのまま残り、次の試行と繋がる。
                let now_us = base_us + timeline.now_us();
                shared.push(&releases(&held, now_us), now_us);
                return reason;
            }
        };

        // 壊れたレポートは捨てる。補間すると存在しない F 数が出る。
        let Some(state) = decode(device, report.bytes()) else {
            continue;
        };

        let mut events = timeline.push(state, report.host_qpc_us());
        for event in &mut events {
            event.at_us += base_us;
        }
        track_held(&mut held, &events);

        shared.push(&events, base_us + timeline.now_us());

        let mut status = shared.lock_status();
        status.scale_us_per_tick = Some(timeline.scale_us_per_tick());
        status.scale_warning = timeline.scale_warning();
    }
}

/// 押されているものを追う。[`History`] の中の状態は読めないので、ここで別に持つ。
fn track_held(held: &mut Vec<Target>, events: &[InputEvent]) {
    for event in events {
        match event.kind {
            EventKind::Press => {
                if !held.contains(&event.target) {
                    held.push(event.target);
                }
            }
            EventKind::Release => held.retain(|target| *target != event.target),
        }
    }
}

fn releases(held: &[Target], at_us: u64) -> Vec<InputEvent> {
    held.iter()
        .map(|target| InputEvent {
            kind: EventKind::Release,
            target: *target,
            at_us,
            // 離しを作ったのはデバイスではなく切断である。時間差は信用できない。
            gap_before: true,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report_decode::Buttons;

    fn press(target: Target) -> InputEvent {
        InputEvent {
            kind: EventKind::Press,
            target,
            at_us: 0,
            gap_before: false,
        }
    }

    #[test]
    fn track_held_follows_press_and_release() {
        let mut held = Vec::new();
        track_held(&mut held, &[press(Target::Button(Buttons::L1))]);
        track_held(&mut held, &[press(Target::Button(Buttons::R1))]);
        // 同じ入力の押下が重なっても増えない。
        track_held(&mut held, &[press(Target::Button(Buttons::L1))]);
        assert_eq!(
            held,
            vec![Target::Button(Buttons::L1), Target::Button(Buttons::R1)]
        );

        track_held(
            &mut held,
            &[InputEvent {
                kind: EventKind::Release,
                ..press(Target::Button(Buttons::L1))
            }],
        );
        assert_eq!(held, vec![Target::Button(Buttons::R1)]);
    }

    /// 切断で作る離しは、押されていたものを漏れなく閉じる。
    #[test]
    fn releases_close_every_held_target() {
        let held = vec![Target::Button(Buttons::L1), Target::Button(Buttons::R1)];
        let events = releases(&held, 1_234);

        assert_eq!(events.len(), 2);
        assert!(events
            .iter()
            .all(|event| event.kind == EventKind::Release && event.at_us == 1_234));
        assert!(releases(&[], 0).is_empty());
    }

    fn release(target: Target, at_us: u64) -> InputEvent {
        InputEvent {
            kind: EventKind::Release,
            at_us,
            ..press(target)
        }
    }

    /// 押して離し、`gap_us` 空けてもう一度押す。区切りが効いたかを試行数で見る。
    fn trials_after_a_gap(config: &Config, gap_us: u64) -> usize {
        let shared = Shared::new(config);
        let first_release_us = 1_000;

        shared.push(&[press(Target::Button(Buttons::L1))], 0);
        shared.push(
            &[release(Target::Button(Buttons::L1), first_release_us)],
            first_release_us,
        );

        let second_press_us = first_release_us + gap_us;
        shared.push(
            &[InputEvent {
                at_us: second_press_us,
                ..press(Target::Button(Buttons::R1))
            }],
            second_press_us,
        );

        shared.snapshot(&ui::Settings::default()).len()
    }

    /// 区切りの間隔が設定から届く。
    #[test]
    fn a_shorter_configured_trial_gap_splits_the_trial() {
        let config = Config {
            trial_gap_ms: 50,
            ..Config::default()
        };

        assert_eq!(trials_after_a_gap(&config, 60_000), 2);
    }

    /// 同じ間隔でも既定の区切り (300ms) では割れない。設定が効いていることの対になる。
    #[test]
    fn the_same_gap_does_not_split_under_the_default_configuration() {
        assert_eq!(trials_after_a_gap(&Config::default(), 60_000), 1);
    }

    /// 表示の設定が設定ファイルから届く。
    #[test]
    fn the_display_settings_come_from_the_configuration() {
        let config = Config {
            fps: 30.0,
            trials_shown: 12,
            ..Config::default()
        };

        let settings = settings_from(&config);

        assert_eq!(settings.fps, 30.0);
        assert_eq!(settings.trials_shown, 12);
    }

    /// 履歴は接続をまたいで残り、次の接続の入力は別の試行になる。
    /// 実機を繋がずに、入力スレッドが履歴へ書く手順だけを再現して確かめる。
    #[test]
    fn a_reconnect_starts_a_new_trial_and_keeps_the_old_one() {
        let shared = Shared::new(&Config::default());
        let first_us = 1_000;
        shared.push(&[press(Target::Button(Buttons::L1))], 0);
        shared.push(
            &[InputEvent {
                kind: EventKind::Release,
                at_us: first_us,
                ..press(Target::Button(Buttons::L1))
            }],
            first_us,
        );

        // 再接続。デバイス時刻は 0 に戻るので、履歴の末尾から区切り分だけ空けて載せ直す。
        let base_us = shared.now_us() + trial_gap_us(&Config::default());
        shared.push(
            &[InputEvent {
                at_us: base_us,
                ..press(Target::Button(Buttons::R1))
            }],
            base_us,
        );

        let trials = shared.snapshot(&ui::Settings::default());
        assert_eq!(trials.len(), 2);
        assert_eq!(trials[0].origin_us, base_us);
        assert_eq!(trials[1].origin_us, 0);
    }
}
