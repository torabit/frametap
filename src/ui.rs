//! [`History`] の読み出し結果を egui で描く。
//!
//! 直近の試行を 1 本の縦リストに並べる。古い試行が上、新しい行が下に増え、縦スクロールは
//! 底に追従する。プレイ中に横目で読むため、視線を動かす向きを一定にする。
//!
//! このファイルは入力の取得も生バイトの復号も知らない。入力の表示名は
//! [`Target::display_name`] から受け取り、ボタンのビット表現をここに持ち込まない。
//!
//! [`Target::display_name`]: crate::timeline::Target::display_name
//!
//! [`History`]: crate::history::History

use egui::{Color32, Frame, RichText, ScrollArea, Ui};

use crate::history::{History, Trial};

/// F 換算の基準の既定値。対象は Dark Souls Remastered の 60fps。
pub const DEFAULT_FPS: f64 = 60.0;

/// 縦リストに積む試行数の既定値。
pub const DEFAULT_TRIALS_SHOWN: usize = 5;

/// 離した区間を行にするかの既定値。
pub const DEFAULT_SHOW_RELEASED: bool = false;

/// 画面に出す版。実機から返ってくるのは写真なので、どの build を動かしたのかを
/// 写真だけで決められるようにする。問い合わせを 1 往復減らす。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 持続 F をそのまま出す上限。これを超えたら [`HOLD_OVERFLOW_TEXT`] にする。
/// 押しっぱなしの行が桁を増やすと、右端の列幅が動いて他の行の数字が読みにくくなる。
const HOLD_FRAMES_CAP: f64 = 99.0;

/// 上限を超えた持続 F の代わりに出す文字。
pub const HOLD_OVERFLOW_TEXT: &str = "99+";

/// 押下 F と持続 F を出さない行に置く文字。
const BLANK_TEXT: &str = "";

/// USB 接続のレポート間隔。押下 F の量子化誤差はこの幅に収まる。
const USB_REPORT_INTERVAL_US: f64 = 4_000.0;

/// 名前の欄の文字数。同時押し 4 つ (`L1 R1 DP8 Cross` で 15 文字) が収まる。
const NAMES_WIDTH: usize = 20;

/// F の欄の文字数。`99.0` と見出しの `press` が収まる。
const FRAMES_WIDTH: usize = 6;

/// 名前の欄の見出し。
const NAMES_HEADER: &str = "input";

/// 押下 F の欄の見出し。
const PRESS_HEADER: &str = "press";

/// 持続 F の欄の見出し。
const HOLD_HEADER: &str = "hold";

/// 取りこぼしのあった行の背景。
const GAP_FILL: Color32 = Color32::from_rgb(70, 70, 70);

/// fps として受け付ける下限。0 を入れて 1F の長さが無限大になるのを防ぐ。
const MIN_FPS: f64 = 1.0;

/// 表示の設定。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// F 換算の基準。
    pub fps: f64,
    /// 縦リストに積む試行数。
    pub trials_shown: usize,
    /// 何も押していない区間を行にするか。
    pub show_released: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fps: DEFAULT_FPS,
            trials_shown: DEFAULT_TRIALS_SHOWN,
            show_released: DEFAULT_SHOW_RELEASED,
        }
    }
}

impl Settings {
    /// 1F の長さ。
    pub fn frame_us(&self) -> f64 {
        1_000_000.0 / self.fps.max(MIN_FPS)
    }
}

/// 接続の状態。入力スレッドが書き、UI が読む。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// 接続しているデバイスの表示名。未接続なら `None`。
    pub device_name: Option<String>,
    /// 未接続の理由。接続中は `None`。
    pub disconnected_reason: Option<String>,
    /// いま使っているデバイス時刻の単位。レポートを 1 件も受けていなければ `None`。
    pub scale_us_per_tick: Option<f64>,
    /// 校正結果が公称単位から離れているときの警告文。
    pub scale_warning: Option<String>,
}

impl Status {
    /// 接続を開いた直後の状態。
    ///
    /// デバイス時刻の単位は接続ごとに校正し直す。前の接続の値を残すと、校正が終わる前の
    /// 数秒間、いま使っていない単位を使っているように読める。[`Self::disconnected`] も同じ。
    pub fn connected(device_name: String) -> Self {
        Self {
            device_name: Some(device_name),
            ..Self::default()
        }
    }

    /// 接続が切れた状態。
    pub fn disconnected(reason: String) -> Self {
        Self {
            disconnected_reason: Some(reason),
            ..Self::default()
        }
    }
}

/// 縦リストの 1 行。
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// この区間で押されている入力の名前。押した順に並ぶ。空の区間では空になる。
    pub names: Vec<&'static str>,
    /// 試行の最初の押下からの F。空の区間では `None`。
    pub press_frames: Option<f64>,
    /// この区間の長さ。
    pub hold_frames: f64,
    /// 直前に取りこぼしがあったか。
    pub gap_before: bool,
    /// この行が試行の先頭か。手前に区切り線を引く。
    pub starts_trial: bool,
}

/// 新しい順に並んだ試行を、古い順の 1 本の行の列に畳む。
///
/// 押しっぱなしの間は [`Trial::rows`] が行を増やさず末尾の行だけが伸びるので、
/// ここでも行数は変わらず持続 F だけが増える。
pub fn lines(trials: &[Trial], settings: &Settings) -> Vec<Line> {
    let frame_us = settings.frame_us();
    let mut lines = Vec::new();

    for trial in trials.iter().rev() {
        let mut starts_trial = true;
        for row in trial.rows() {
            // 離した区間の行は持続 F しか持たない。読む行数が倍になるだけなので既定では出さない。
            // 入力と入力の間合いは、押下 F の差で読める。
            if row.held.is_empty() && !settings.show_released {
                continue;
            }

            lines.push(Line {
                names: row
                    .held
                    .iter()
                    .map(|target| target.display_name())
                    .collect(),
                press_frames: (!row.held.is_empty())
                    .then(|| row.at_us.saturating_sub(trial.origin_us) as f64 / frame_us),
                hold_frames: row.end_us.saturating_sub(row.at_us) as f64 / frame_us,
                gap_before: row.gap_before,
                starts_trial,
            });
            starts_trial = false;
        }
    }

    lines
}

/// 表示に使う試行を [`History`] から読む。新しい順に最大 [`Settings::trials_shown`] 件。
///
/// 何件読むかは表示の設定なので、その判断をここに置く。呼び出し側は排他を短く握って
/// これを呼び、描画には排他の要らない [`Trial`] の複製を渡す。
pub fn recent_trials(history: &History, settings: &Settings) -> Vec<Trial> {
    history.recent_trials(settings.trials_shown)
}

/// ヘッダと縦リストを描く。`trials` は [`recent_trials`] の戻り値、
/// つまり新しい順に並んだ試行である。
pub fn show(
    ui: &mut Ui,
    trials: &[Trial],
    status: &Status,
    settings: &Settings,
    notices: &[String],
) {
    show_header(ui, status, settings, notices);
    ui.separator();

    // 見出しはスクロール領域の外に置く。リストが流れても列の意味が画面から消えない。
    show_column_headers(ui);
    ui.separator();

    ScrollArea::vertical()
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (index, line) in lines(trials, settings).iter().enumerate() {
                // 試行の境界に線を引く。先頭の行の手前には引かない。
                if line.starts_trial && index > 0 {
                    ui.separator();
                }
                show_line(ui, line);
            }
        });
}

/// 接続の状態と、F 数の読み方に必要な但し書きを出す。
///
/// 但し書きは条件付きで隠さない。1F ずれを読んでいる最中に、表示の幅が
/// どこから来ているかを思い出せる状態にしておく。
fn show_header(ui: &mut Ui, status: &Status, settings: &Settings, notices: &[String]) {
    // 版と接続を同じ行に置く。窓は縦に狭く、但し書きだけで数行を使う。
    ui.horizontal(|ui| {
        ui.label(title_text());

        let text = RichText::new(status_text(status));
        match status.device_name {
            Some(_) => ui.label(text),
            None => ui.label(text.color(Color32::LIGHT_RED)),
        };
    });

    match status.scale_us_per_tick {
        Some(scale) => ui.label(format!("device clock: {scale:.4} us/tick")),
        None => ui.label("device clock: not measured yet"),
    };

    if let Some(warning) = &status.scale_warning {
        ui.label(RichText::new(warning).color(Color32::LIGHT_RED));
    }

    let frame_us = settings.frame_us();
    ui.label(format!(
        "USB reports every 4ms, so each frame count carries +/-{:.2}F of error",
        USB_REPORT_INTERVAL_US / frame_us
    ));
    ui.label(format!(
        "counted on a private {:.0}fps grid, may differ from the game by up to 1F",
        settings.fps.max(MIN_FPS)
    ));

    // 設定が効かなかったことは、隠すと画面から区別できない。出したままにする。
    for notice in notices {
        ui.label(RichText::new(notice).color(Color32::LIGHT_RED));
    }
}

/// どの欄が何かを示す見出し。行と同じ組み方なので、列とずれない。
fn show_column_headers(ui: &mut Ui) {
    ui.label(header_text(&columns(
        NAMES_HEADER,
        PRESS_HEADER,
        HOLD_HEADER,
    )));
}

/// 見出しの文字。数字より小さく弱くして、行の数字から視線を奪わない。
fn header_text(text: &str) -> RichText {
    RichText::new(text).monospace().small().weak()
}

fn show_line(ui: &mut Ui, line: &Line) {
    let frame = if line.gap_before {
        // 取りこぼした区間は時間差を信用できない。行ごと背景で潰して読み飛ばせるようにする。
        Frame::NONE.fill(GAP_FILL)
    } else {
        Frame::NONE
    };

    frame.show(ui, |ui| {
        ui.monospace(row_text(&line.names, line.press_frames, line.hold_frames));
    });
}

/// 1 行ぶんの文字列。
///
/// 3 つの欄を別々の widget にしない。egui は要求した幅ではなく実際に使った矩形のぶんだけ
/// cursor を進めるので、名前の欄に固定幅を渡しても名前が長い行で F の列が右へずれる。
/// 等幅の 1 行に組めば、割り付けを egui に任せずに桁が揃う。
pub fn row_text(names: &[&str], press_frames: Option<f64>, hold_frames: f64) -> String {
    columns(
        &names.join(" "),
        &press_text(press_frames),
        &hold_text(hold_frames),
    )
}

/// 名前を左詰め、F の 2 欄を右詰めにして 1 行にする。
fn columns(names: &str, press: &str, hold: &str) -> String {
    format!("{names:<NAMES_WIDTH$}{press:>FRAMES_WIDTH$}{hold:>FRAMES_WIDTH$}")
}

/// 画面の左上に出す名前と版。
pub fn title_text() -> String {
    format!("frametap {VERSION}")
}

/// 接続の状態を 1 行にする。
pub fn status_text(status: &Status) -> String {
    match (&status.device_name, &status.disconnected_reason) {
        (Some(name), _) => format!("connected: {name}"),
        (None, Some(reason)) => format!("disconnected: {reason}"),
        (None, None) => "disconnected".to_owned(),
    }
}

/// 押下 F の表示。押されていない区間では空欄にする。
pub fn press_text(frames: Option<f64>) -> String {
    match frames {
        Some(frames) => format!("{frames:.1}"),
        None => BLANK_TEXT.to_owned(),
    }
}

/// 持続 F の表示。桁が増えると列幅が動くので、上限を超えたら丸める。
pub fn hold_text(frames: f64) -> String {
    if frames > HOLD_FRAMES_CAP {
        HOLD_OVERFLOW_TEXT.to_owned()
    } else {
        format!("{frames:.1}")
    }
}
