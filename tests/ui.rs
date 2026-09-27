//! [`frametap::ui`] の行の組み立てと表示文字列を、公開 API だけで確かめる。
//!
//! 入力の組み立てには [`frametap::report_decode`] の値が要る。src/ui.rs をこの依存から
//! 切り離すため、テストをここに置く。

use frametap::history::{History, DEFAULT_RETAIN_US, DEFAULT_TRIAL_GAP_US};
use frametap::report_decode::{Buttons, Direction};
use frametap::timeline::{EventKind, InputEvent, Target};
use frametap::ui::{
    hold_text, lines, press_text, recent_trials, show, Settings, Status, DEFAULT_TRIALS_SHOWN,
    HOLD_OVERFLOW_TEXT,
};

/// 60fps の 1F。
const FRAME_US: u64 = 16_667;

fn history() -> History {
    History::new(DEFAULT_RETAIN_US, DEFAULT_TRIAL_GAP_US)
}

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

/// 1F ずらしの `L1 + ↑` と `R1`。読み取りたい形をそのまま組む。
fn wrong_warp_history() -> History {
    let mut history = history();
    history.push(
        &[
            press(Target::Button(Buttons::L1), 0),
            press(Target::Dpad(Direction::N), 0),
        ],
        0,
    );
    history.push(&[press(Target::Button(Buttons::R1), FRAME_US)], FRAME_US);
    history
}

/// 押下 F は試行の起点からの差、持続 F は行の長さ。1F ずらしが 1.0 と読める。
#[test]
fn frames_count_from_the_trial_origin() {
    let mut history = wrong_warp_history();
    history.push(&[], 3 * FRAME_US);

    let lines = lines(&history.recent_trials(5), &Settings::default());
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].names, vec!["L1", "D↑"]);
    assert_eq!(lines[0].press_frames, Some(0.0));
    assert_eq!(lines[1].names, vec!["L1", "D↑", "R1"]);
    assert_eq!(press_text(lines[1].press_frames), "1.0");
    assert_eq!(hold_text(lines[1].hold_frames), "2.0");
}

/// 長押しの間は行が増えず、最後の行の持続 F だけが伸びる。
#[test]
fn holding_grows_the_last_line_without_adding_lines() {
    let mut history = wrong_warp_history();

    let mut previous = Vec::new();
    for step in 2..=10u64 {
        history.push(&[], step * FRAME_US);
        let lines = lines(&history.recent_trials(5), &Settings::default());

        assert_eq!(lines.len(), 2, "{step}F 目");
        if let Some(previous) = previous.first() {
            // 増えるのは末尾の長さだけ。手前の行は動かない。
            assert_eq!(&lines[0], previous, "{step}F 目");
        }
        // FRAME_US は 16666.7µs を丸めた値なので、表示に使う桁で比べる。
        assert_eq!(
            hold_text(lines[1].hold_frames),
            format!("{:.1}", (step - 1) as f64),
            "{step}F 目"
        );
        previous = lines;
    }
}

/// 試行は古い順に並び、先頭の行だけが区切りを持つ。
#[test]
fn lines_run_oldest_first_and_mark_each_trial_head() {
    let mut history = history();
    for index in 0..3u64 {
        let at_us = index * 10 * DEFAULT_TRIAL_GAP_US;
        history.push(&[press(Target::Button(Buttons::CROSS), at_us)], at_us);
        history.push(
            &[release(Target::Button(Buttons::CROSS), at_us + FRAME_US)],
            at_us + FRAME_US,
        );
    }
    history.push(&[], 30 * DEFAULT_TRIAL_GAP_US);

    let trials = history.recent_trials(DEFAULT_TRIALS_SHOWN);
    let lines = lines(&trials, &Settings::default());

    // 各試行は押下の行と離した後の空の行を持つ。
    assert_eq!(
        lines
            .iter()
            .map(|line| line.starts_trial)
            .collect::<Vec<bool>>(),
        vec![true, false, true, false, true, false],
    );
    // 古い試行が先。試行内の押下 F は起点からの差なので、どの試行でも 0.0 から始まる。
    assert_eq!(lines[0].press_frames, Some(0.0));
    assert_eq!(lines[0].names, vec!["Cross"]);
    // 空の区間は名前も押下 F も持たず、長さだけを出す。
    assert!(lines[1].names.is_empty());
    assert_eq!(press_text(lines[1].press_frames), "");
}

/// 試行数の上限は [`frametap::ui::recent_trials`] が [`Settings`] から読む。新しい方を残す。
#[test]
fn trials_shown_caps_the_list() {
    let mut history = history();
    for index in 0..4u64 {
        let at_us = index * 10 * DEFAULT_TRIAL_GAP_US;
        history.push(&[press(Target::Button(Buttons::CROSS), at_us)], at_us);
        history.push(
            &[release(Target::Button(Buttons::CROSS), at_us + FRAME_US)],
            at_us + FRAME_US,
        );
    }
    history.push(&[], 40 * DEFAULT_TRIAL_GAP_US);

    let heads = |shown: usize| {
        let settings = Settings {
            trials_shown: shown,
            ..Settings::default()
        };
        lines(&recent_trials(&history, &settings), &settings)
            .iter()
            .filter(|line| line.starts_trial)
            .count()
    };
    assert_eq!(heads(4), 4);
    assert_eq!(heads(2), 2);
}

/// fps を変えると 1F の長さが変わる。30fps では同じ差が半分の F になる。
#[test]
fn fps_changes_the_frame_length() {
    let mut history = history();
    history.push(&[press(Target::Button(Buttons::L1), 0)], 0);
    history.push(
        &[press(Target::Button(Buttons::R1), 33_333)],
        33_333 + FRAME_US,
    );

    let frames = |fps: f64| {
        let settings = Settings {
            fps,
            ..Settings::default()
        };
        lines(&history.recent_trials(1), &settings)[1]
            .press_frames
            .expect("押下のある行")
    };

    assert_eq!(format!("{:.1}", frames(60.0)), "2.0");
    assert_eq!(format!("{:.1}", frames(30.0)), "1.0");
}

/// 取りこぼしの印は行に伝わる。
#[test]
fn gap_before_reaches_the_line() {
    let mut history = history();
    history.push(&[press(Target::Button(Buttons::L1), 0)], 0);
    history.push(
        &[InputEvent {
            gap_before: true,
            ..press(Target::Button(Buttons::R1), FRAME_US)
        }],
        FRAME_US,
    );
    history.push(&[], 2 * FRAME_US);

    let lines = lines(&history.recent_trials(1), &Settings::default());
    assert!(!lines[0].gap_before);
    assert!(lines[1].gap_before);
}

/// 持続 F は 99.0 を超えたら丸める。境界の 99.0 はそのまま出す。
#[test]
fn hold_text_caps_above_ninety_nine() {
    assert_eq!(hold_text(0.0), "0.0");
    assert_eq!(hold_text(98.9), "98.9");
    assert_eq!(hold_text(99.0), "99.0");
    // 丸めた表示ではなく値で判定する。99.04 は "99.0" にならず丸めた印になる。
    assert_eq!(hold_text(99.04), HOLD_OVERFLOW_TEXT);
}

/// 接続の状態は、その接続で校正した単位だけを持つ。前の接続の値は残さない。
#[test]
fn a_new_connection_starts_without_a_scale() {
    let connected = Status::connected("DualSense".to_owned());
    assert_eq!(connected.device_name.as_deref(), Some("DualSense"));
    assert_eq!(connected.scale_us_per_tick, None);
    assert_eq!(connected.scale_warning, None);

    let disconnected = Status::disconnected("読みが止まった".to_owned());
    assert_eq!(disconnected.device_name, None);
    assert_eq!(disconnected.scale_us_per_tick, None);
    assert_eq!(disconnected.scale_warning, None);
}

/// 描画がウィンドウ無しで最後まで通る。列の確保と幅の計算が壊れていれば panic する。
#[test]
fn show_runs_without_a_window() {
    let mut history = wrong_warp_history();
    history.push(&[], 5 * FRAME_US);
    let trials = history.recent_trials(DEFAULT_TRIALS_SHOWN);

    let connected = Status {
        scale_us_per_tick: Some(0.333),
        scale_warning: Some("単位が公称値から外れている".to_owned()),
        ..Status::connected("DualSense".to_owned())
    };

    for status in [connected, Status::default()] {
        egui::__run_test_ui(|ui| show(ui, &trials, &status, &Settings::default()));
    }
}
