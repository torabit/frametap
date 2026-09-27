//! [`PadState`] の列を [`InputEvent`] の列に変換する。デバイス時刻の校正と wrap 処理もここで行う。
//!
//! 時刻はレポート内のデバイス時刻を一次に使い、ホストの QPC は wrap 回数の曖昧さの解消と
//! スケールの校正にだけ使う。この役割分担を崩すと、QPC のジッタが 1F の判定に混ざる。
//!
//! 出力する時刻は最初のレポートを 0 とする連続値である。取りこぼしても補間はせず、
//! 取りこぼしの直後に出たイベントに [`InputEvent::gap_before`] を立てて下流に伝える。

use crate::report_decode::{Buttons, Device, Direction, PadState};

/// イベントの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Press,
    Release,
}

/// イベントが指す入力。十字キーと左スティックは別物として扱う。
/// どちらで方向を入れたかが見えないと、ずれの原因を切り分けられない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// 単一のボタン。複数ビットが立った値は入れない。
    Button(Buttons),
    Dpad(Direction),
    /// 左スティックを 8 方向に量子化したもの。
    Stick(Direction),
}

/// 押下または離しの 1 件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputEvent {
    pub kind: EventKind,
    pub target: Target,
    /// 最初のレポートを 0 とする経過µs。
    pub at_us: u64,
    /// 直前に取りこぼしがあったか。立っているとき、このイベントの時刻差は信用できない。
    pub gap_before: bool,
}

/// DualSense の公称単位。
const DUALSENSE_NOMINAL_US_PER_TICK: f64 = 0.33;

/// DualShock 4 の公称単位。
const DUALSHOCK4_NOMINAL_US_PER_TICK: f64 = 5.33;

/// デバイス時刻の幅。DualSense は 32bit、DualShock 4 は 16bit で回る。
const DUALSENSE_WRAP_PERIOD: u64 = 1 << 32;
const DUALSHOCK4_WRAP_PERIOD: u64 = 1 << 16;

/// 連番の法。DualShock 4 は 6bit しか持たない。
const DUALSENSE_SEQ_MODULUS: u16 = 256;
const DUALSHOCK4_SEQ_MODULUS: u16 = 64;

/// 校正に使うホスト時間の長さ。
const CALIBRATION_WINDOW_US: u64 = 2_000_000;

/// 公称単位からこの割合以上外れたら警告する。黙って定数倍ずれた F 数を出すのが最悪である。
const SCALE_WARNING_RATIO: f64 = 0.20;

/// 量子化の 1 方向が占める角度。
const SECTOR_DEG: f32 = 45.0;

/// 生値の中心。0..=255 の中点を取ると ±1.0 に正規化できる。
const STICK_CENTER: f32 = 127.5;

/// 角度 0 度から 45 度刻みで反時計回りに並ぶ方向。Y 軸は上向き。
const STICK_DIRECTIONS: [Direction; 8] = [
    Direction::E,
    Direction::NE,
    Direction::N,
    Direction::NW,
    Direction::W,
    Direction::SW,
    Direction::S,
    Direction::SE,
];

/// 方向を [`Target`] に包む構築子。十字キーとスティックで同じ差分処理を使うために型を揃える。
type DirectionTarget = fn(Direction) -> Target;

/// 直前のレポートから引き継ぐもの。
#[derive(Debug, Clone, Copy)]
struct Previous {
    device_ts: u32,
    host_qpc_us: u64,
    seq: u8,
    buttons: Buttons,
    dpad: Option<Direction>,
    stick: Option<Direction>,
}

/// レポート列をイベント列に変換する状態機械。
///
/// 最初のレポートは基準を作るだけでイベントを出さない。以降は差分だけを出す。
#[derive(Debug)]
pub struct Timeline {
    device: Device,
    deadzone: f32,
    hysteresis_deg: f32,
    scale_us_per_tick: f64,
    /// 校正が済むまでは公称単位を使う。
    calibrated: bool,
    scale_warning: Option<String>,
    /// 最初のレポートの QPC。校正のホスト側の起点になる。
    host_start_us: u64,
    /// 最初のレポートからの累積 tick。wrap を解いた値を足す。校正の分母になる。
    elapsed_ticks: u64,
    /// 最初のレポートからの経過µs。µs に丸めた差分を足すと 1 件あたり 1µs 未満の誤差が
    /// 250Hz で積もるため、累積の側を実数で持つ。
    elapsed_us: f64,
    previous: Option<Previous>,
}

impl Timeline {
    /// `deadzone` は正規化した中心からの距離、`hysteresis_deg` は方向を切り替える角度の閾値。
    /// `hysteresis_deg` が小さいほど現在の方向を長く保つ。
    pub fn new(device: Device, deadzone: f32, hysteresis_deg: f32) -> Self {
        Self {
            device,
            deadzone: deadzone.max(0.0),
            hysteresis_deg: hysteresis_deg.clamp(0.0, 180.0),
            scale_us_per_tick: nominal_us_per_tick(device),
            calibrated: false,
            scale_warning: None,
            host_start_us: 0,
            elapsed_ticks: 0,
            elapsed_us: 0.0,
            previous: None,
        }
    }

    /// レポート 1 件を取り込み、そのレポートが起こしたイベントを返す。
    ///
    /// `host_qpc_us` は受信直後に打った QPC のµs 値。状態が変わらないレポートでも時刻は進む。
    pub fn push(&mut self, state: PadState, host_qpc_us: u64) -> Vec<InputEvent> {
        let Some(previous) = self.previous else {
            self.host_start_us = host_qpc_us;
            self.previous = Some(Previous {
                device_ts: state.device_ts,
                host_qpc_us,
                seq: state.seq,
                buttons: state.buttons,
                dpad: state.dpad,
                stick: self.quantize_stick(state.left_stick, None),
            });
            return Vec::new();
        };

        let stick = self.quantize_stick(state.left_stick, previous.stick);
        let host_delta_us = host_qpc_us.saturating_sub(previous.host_qpc_us);
        let delta_ticks =
            self.unwrapped_delta_ticks(previous.device_ts, state.device_ts, host_delta_us);

        self.elapsed_ticks = self.elapsed_ticks.saturating_add(delta_ticks);
        self.elapsed_us += delta_ticks as f64 * self.scale_us_per_tick;

        let gap_before = self.has_seq_gap(previous.seq, state.seq);
        let events = self.diff(&previous, state.buttons, state.dpad, stick, gap_before);

        self.previous = Some(Previous {
            device_ts: state.device_ts,
            host_qpc_us,
            seq: state.seq,
            buttons: state.buttons,
            dpad: state.dpad,
            stick,
        });

        self.calibrate(host_qpc_us);

        events
    }

    /// 最初のレポートからの経過µs。
    pub fn now_us(&self) -> u64 {
        // f64 から u64 への cast は飽和するので、桁が溢れても panic しない。
        self.elapsed_us.round() as u64
    }

    /// 現在使っているデバイス時刻の単位。校正前は公称値。
    pub fn scale_us_per_tick(&self) -> f64 {
        self.scale_us_per_tick
    }

    /// 校正結果が公称単位から離れているときの警告文。
    pub fn scale_warning(&self) -> Option<String> {
        self.scale_warning.clone()
    }

    /// wrap を解いた tick 差を返す。
    ///
    /// 非負の剰余差に wrap 周期の整数倍を足した候補のうち、ホストの経過時間を現在のスケールで
    /// tick に換算した値に最も近いものを選ぶ。DualShock 4 は 349ms で回るので、
    /// 無入力が続くと剰余差だけでは wrap 回数が決まらない。
    fn unwrapped_delta_ticks(&self, previous_ts: u32, device_ts: u32, host_delta_us: u64) -> u64 {
        let period = wrap_period(self.device);
        let raw =
            (u64::from(device_ts) % period + period - u64::from(previous_ts) % period) % period;

        let host_ticks = host_delta_us as f64 / self.scale_us_per_tick;
        let wraps = ((host_ticks - raw as f64) / period as f64).round().max(0.0) as u64;

        raw.saturating_add(wraps.saturating_mul(period))
    }

    /// ホスト時間で 2 秒分のレポートが溜まったらスケールを推定する。
    fn calibrate(&mut self, host_qpc_us: u64) {
        if self.calibrated {
            return;
        }

        let host_elapsed_us = host_qpc_us.saturating_sub(self.host_start_us);
        if host_elapsed_us < CALIBRATION_WINDOW_US || self.elapsed_ticks == 0 {
            return;
        }

        let estimated = host_elapsed_us as f64 / self.elapsed_ticks as f64;
        if !estimated.is_finite() || estimated <= 0.0 {
            return;
        }

        self.calibrated = true;
        self.scale_us_per_tick = estimated;

        let nominal = nominal_us_per_tick(self.device);
        let deviation = (estimated - nominal).abs() / nominal;
        if deviation >= SCALE_WARNING_RATIO {
            self.scale_warning = Some(format!(
                "デバイス時刻の単位が公称値から {:.0}% 外れている (推定 {estimated:.3}µs/tick、公称 {nominal:.2}µs/tick)",
                deviation * 100.0
            ));
        }
    }

    /// 連番が飛んでいるか。法はデバイスによって違う。
    fn has_seq_gap(&self, previous_seq: u8, seq: u8) -> bool {
        let modulus = seq_modulus(self.device);
        let expected = (u16::from(previous_seq) + 1) % modulus;
        u16::from(seq) % modulus != expected
    }

    /// 直前の状態との差分をイベントにする。同じレポートから出るイベントは同じ時刻を持つ。
    fn diff(
        &self,
        previous: &Previous,
        buttons: Buttons,
        dpad: Option<Direction>,
        stick: Option<Direction>,
        gap_before: bool,
    ) -> Vec<InputEvent> {
        let mut events = Vec::new();
        let at_us = self.now_us();
        let mut emit = |kind, target| {
            events.push(InputEvent {
                kind,
                target,
                at_us,
                gap_before,
            })
        };

        for button in (previous.buttons ^ buttons).iter() {
            let kind = if buttons.contains(button) {
                EventKind::Press
            } else {
                EventKind::Release
            };
            emit(kind, Target::Button(button));
        }

        // 方向は 1 つしか取れないので、変わったら古い方の離しを出してから新しい方を押す。
        for (was, now, target) in [
            (previous.dpad, dpad, Target::Dpad as DirectionTarget),
            (previous.stick, stick, Target::Stick as DirectionTarget),
        ] {
            if was == now {
                continue;
            }
            if let Some(direction) = was {
                emit(EventKind::Release, target(direction));
            }
            if let Some(direction) = now {
                emit(EventKind::Press, target(direction));
            }
        }

        events
    }

    /// 左スティックの生値を 8 方向に量子化する。
    ///
    /// デッドゾーンの判定は現在の方向に依らない。方向の切り替えだけがヒステリシスを持ち、
    /// 隣のセクタに入っても、その方向の中心から `hysteresis_deg` 以内に来るまで現在の方向を保つ。
    fn quantize_stick(
        &self,
        left_stick: (u8, u8),
        current: Option<Direction>,
    ) -> Option<Direction> {
        let x = (f32::from(left_stick.0) - STICK_CENTER) / STICK_CENTER;
        // 生値の Y は下向きに増えるので反転して Y 上向きに揃える。
        let y = -(f32::from(left_stick.1) - STICK_CENTER) / STICK_CENTER;

        if x.hypot(y) < self.deadzone {
            return None;
        }

        let angle_deg = y.atan2(x).to_degrees();
        let index = (angle_deg / SECTOR_DEG).round() as i32;
        let candidate = STICK_DIRECTIONS[index.rem_euclid(STICK_DIRECTIONS.len() as i32) as usize];

        let Some(current) = current else {
            return Some(candidate);
        };
        if candidate == current {
            return Some(current);
        }

        if angle_gap_deg(angle_deg, direction_center_deg(candidate)) <= self.hysteresis_deg {
            Some(candidate)
        } else {
            Some(current)
        }
    }
}

fn nominal_us_per_tick(device: Device) -> f64 {
    match device {
        Device::DualSense => DUALSENSE_NOMINAL_US_PER_TICK,
        Device::DualShock4 => DUALSHOCK4_NOMINAL_US_PER_TICK,
    }
}

fn wrap_period(device: Device) -> u64 {
    match device {
        Device::DualSense => DUALSENSE_WRAP_PERIOD,
        Device::DualShock4 => DUALSHOCK4_WRAP_PERIOD,
    }
}

fn seq_modulus(device: Device) -> u16 {
    match device {
        Device::DualSense => DUALSENSE_SEQ_MODULUS,
        Device::DualShock4 => DUALSHOCK4_SEQ_MODULUS,
    }
}

/// 方向が代表する角度。Y 上向きで E が 0 度。
fn direction_center_deg(direction: Direction) -> f32 {
    let index = STICK_DIRECTIONS
        .iter()
        .position(|candidate| *candidate == direction)
        .unwrap_or(0);
    index as f32 * SECTOR_DEG
}

/// 2 つの角度の差を 0..=180 度で返す。
fn angle_gap_deg(a_deg: f32, b_deg: f32) -> f32 {
    let diff = (a_deg - b_deg).rem_euclid(360.0);
    if diff > 180.0 {
        360.0 - diff
    } else {
        diff
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 中心の生値。
    const CENTER: u8 = 128;

    /// USB のレポート間隔。
    const REPORT_INTERVAL_US: u64 = 4_000;

    /// 1F。
    const FRAME_US: u64 = 16_667;

    fn state(device_ts: u32, seq: u8) -> PadState {
        PadState {
            buttons: Buttons::empty(),
            dpad: None,
            left_stick: (CENTER, CENTER),
            device_ts,
            seq,
        }
    }

    fn with_buttons(device_ts: u32, seq: u8, buttons: Buttons) -> PadState {
        PadState {
            buttons,
            ..state(device_ts, seq)
        }
    }

    fn with_dpad(device_ts: u32, seq: u8, dpad: Option<Direction>) -> PadState {
        PadState {
            dpad,
            ..state(device_ts, seq)
        }
    }

    fn with_stick(device_ts: u32, seq: u8, left_stick: (u8, u8)) -> PadState {
        PadState {
            left_stick,
            ..state(device_ts, seq)
        }
    }

    /// 単位円上の角度から左スティックの生値を作る。Y 上向きの角度を渡す。
    fn stick_at(angle_deg: f32, magnitude: f32) -> (u8, u8) {
        let radians = angle_deg.to_radians();
        let x = STICK_CENTER + radians.cos() * magnitude * STICK_CENTER;
        let y = STICK_CENTER - radians.sin() * magnitude * STICK_CENTER;
        (x.round() as u8, y.round() as u8)
    }

    fn dualsense() -> Timeline {
        Timeline::new(Device::DualSense, 0.5, 15.0)
    }

    fn dualshock4() -> Timeline {
        Timeline::new(Device::DualShock4, 0.5, 15.0)
    }

    /// 公称単位から tick 数を求める。
    fn dualsense_ticks(us: u64) -> u32 {
        (us as f64 / DUALSENSE_NOMINAL_US_PER_TICK).round() as u32
    }

    fn dualshock4_ticks(us: u64) -> u32 {
        (us as f64 / DUALSHOCK4_NOMINAL_US_PER_TICK).round() as u32
    }

    /// 校正が済むまで、`us_per_tick` の単位で時刻を打つ無入力のレポートを流す。
    /// デバイス時刻は実機と同じく wrap 周期で折り返した値を渡す。
    /// 最後に流したホスト時刻と連番を返す。
    fn feed_until_calibrated(timeline: &mut Timeline, us_per_tick: f64) -> (u64, u8) {
        let period = wrap_period(timeline.device);
        let mut host_us = 0;
        let mut seq = 0u8;

        while host_us < CALIBRATION_WINDOW_US {
            host_us += REPORT_INTERVAL_US;
            seq = seq.wrapping_add(1);
            let ticks = (host_us as f64 / us_per_tick).round() as u64 % period;
            timeline.push(state(ticks as u32, seq), host_us);
        }

        (host_us, seq)
    }

    /// DualShock 4 が `wraps` 周するあいだ無入力で、その後にボタンを押したときの誤差µs。
    fn dualshock4_wrap_error_us(wraps: u64) -> i64 {
        let period_us =
            (DUALSHOCK4_WRAP_PERIOD as f64 * DUALSHOCK4_NOMINAL_US_PER_TICK).round() as u64;
        let elapsed_us = period_us * wraps + 20_000;

        let mut timeline = dualshock4();
        timeline.push(state(1_000, 0), 0);

        let device_ts = (1_000 + u64::from(dualshock4_ticks(elapsed_us))) % DUALSHOCK4_WRAP_PERIOD;
        let events = timeline.push(
            with_buttons(device_ts as u32, 1, Buttons::CROSS),
            elapsed_us,
        );

        assert_eq!(events.len(), 1);
        events[0].at_us as i64 - elapsed_us as i64
    }

    #[test]
    fn first_report_only_sets_baseline() {
        let mut timeline = dualsense();

        let events = timeline.push(with_buttons(12_345, 7, Buttons::L1), 1_000_000);

        assert!(events.is_empty());
        assert_eq!(timeline.now_us(), 0);
    }

    #[test]
    fn uses_nominal_scale_before_calibration() {
        let mut timeline = dualsense();
        timeline.push(state(0, 0), 0);

        let events = timeline.push(
            with_buttons(dualsense_ticks(FRAME_US), 1, Buttons::R1),
            REPORT_INTERVAL_US,
        );

        assert_eq!(timeline.scale_us_per_tick(), DUALSENSE_NOMINAL_US_PER_TICK);
        assert_eq!(events.len(), 1);
        // 公称 0.33µs/tick で 1F 分の tick を入れたので 1F 付近に出る。
        assert!((events[0].at_us as i64 - FRAME_US as i64).abs() <= 1);
        assert_eq!(timeline.now_us(), events[0].at_us);
    }

    /// デバイスが公称の 3 倍の単位で時刻を打つ個体を模す。校正後はホスト時間に一致する。
    #[test]
    fn calibrates_scale_and_warns_on_triple_unit() {
        let actual_us_per_tick = DUALSENSE_NOMINAL_US_PER_TICK * 3.0;
        let mut timeline = dualsense();
        timeline.push(state(0, 0), 0);

        let (host_us, seq) = feed_until_calibrated(&mut timeline, actual_us_per_tick);

        let estimated = timeline.scale_us_per_tick();
        assert!(
            (estimated - actual_us_per_tick).abs() < 0.01,
            "推定 {estimated}"
        );
        assert!(timeline.scale_warning().is_some());

        // 校正後の 1F は、実単位で数えた tick 差に対して 1F として出る。
        let before = timeline.now_us();
        let ticks = ((host_us + FRAME_US) as f64 / actual_us_per_tick).round() as u32;
        let events = timeline.push(
            with_buttons(ticks, seq.wrapping_add(1), Buttons::L1),
            host_us + FRAME_US,
        );

        let measured = events[0].at_us - before;
        assert!(
            (measured as i64 - FRAME_US as i64).abs() <= 2,
            "計測 {measured}µs"
        );
    }

    /// 公称どおりの DualSense では警告を出さない。
    #[test]
    fn does_not_warn_when_dualsense_scale_matches_nominal() {
        let mut timeline = dualsense();
        timeline.push(state(0, 0), 0);

        feed_until_calibrated(&mut timeline, DUALSENSE_NOMINAL_US_PER_TICK);

        assert!(
            (timeline.scale_us_per_tick() - DUALSENSE_NOMINAL_US_PER_TICK).abs() < 0.01,
            "推定 {}",
            timeline.scale_us_per_tick()
        );
        assert_eq!(timeline.scale_warning(), None);
    }

    /// DualShock 4 でも同じ。校正は 349ms で回るデバイス時刻の上でも成立する。
    #[test]
    fn does_not_warn_when_dualshock4_scale_matches_nominal() {
        let mut timeline = dualshock4();
        timeline.push(state(0, 0), 0);

        let (host_us, seq) = feed_until_calibrated(&mut timeline, DUALSHOCK4_NOMINAL_US_PER_TICK);

        assert!(
            (timeline.scale_us_per_tick() - DUALSHOCK4_NOMINAL_US_PER_TICK).abs() < 0.01,
            "推定 {}",
            timeline.scale_us_per_tick()
        );
        assert_eq!(timeline.scale_warning(), None);

        // 校正後の 1F が 1F として出る。wrap をまたいでも単位は変わらない。
        let before = timeline.now_us();
        let ticks = u64::from(dualshock4_ticks(host_us + FRAME_US)) % DUALSHOCK4_WRAP_PERIOD;
        let events = timeline.push(
            with_buttons(ticks as u32, seq.wrapping_add(1), Buttons::L1),
            host_us + FRAME_US,
        );

        let measured = events[0].at_us - before;
        assert!(
            (measured as i64 - FRAME_US as i64).abs() <= 6,
            "計測 {measured}µs"
        );
    }

    /// DualShock 4 は 349ms で回る。1 周をまたぐ無入力でも QPC から wrap 回数が決まる。
    #[test]
    fn resolves_one_dualshock4_wrap_with_host_clock() {
        let error = dualshock4_wrap_error_us(1).abs();

        assert!(error <= 6, "誤差 {error}µs");
    }

    /// 2 周。剰余差は 1 周のときと同じで、QPC だけが両者を分ける。
    #[test]
    fn resolves_two_dualshock4_wraps_with_host_clock() {
        let error = dualshock4_wrap_error_us(2).abs();

        assert!(error <= 6, "誤差 {error}µs");
    }

    /// 5 周。wrap 回数が増えても誤差は tick の丸め分しか積もらない。
    #[test]
    fn resolves_five_dualshock4_wraps_with_host_clock() {
        let error = dualshock4_wrap_error_us(5).abs();

        assert!(error <= 6, "誤差 {error}µs");
    }

    /// 1 周に満たない間隔では wrap を足さない。
    #[test]
    fn keeps_delta_within_one_period() {
        let mut timeline = dualshock4();
        timeline.push(state(65_000, 0), 0);

        // 65_000 + 1000 tick で 16bit を跨ぐ。剰余差は正しく 1000 tick になる。
        let elapsed_us = (1_000.0 * DUALSHOCK4_NOMINAL_US_PER_TICK).round() as u64;
        let events = timeline.push(with_buttons(464, 1, Buttons::CROSS), elapsed_us);

        assert_eq!(events[0].at_us, elapsed_us);
    }

    /// DualShock 4 の 1 周期を超える 400ms の無入力。剰余差だけでは 51ms と区別できない。
    #[test]
    fn measures_400ms_gap_across_dualshock4_wrap() {
        let gap_us = 400_000;
        let mut timeline = dualshock4();
        timeline.push(with_buttons(0, 0, Buttons::L1), 0);

        let device_ts = u64::from(dualshock4_ticks(gap_us)) % DUALSHOCK4_WRAP_PERIOD;
        let events = timeline.push(with_buttons(device_ts as u32, 1, Buttons::empty()), gap_us);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::Release);
        assert_eq!(events[0].target, Target::Button(Buttons::L1));
        assert!((events[0].at_us as i64 - gap_us as i64).abs() <= 6);
    }

    /// 同じ 400ms を DualSense で測る。こちらは wrap しない。
    #[test]
    fn measures_400ms_gap_on_dualsense() {
        let gap_us = 400_000;
        let mut timeline = dualsense();
        timeline.push(state(0, 0), 0);

        let events = timeline.push(
            with_buttons(dualsense_ticks(gap_us), 1, Buttons::TRIANGLE),
            gap_us,
        );

        assert!((events[0].at_us as i64 - gap_us as i64).abs() <= 1);
    }

    #[test]
    fn flags_gap_on_events_of_the_report_after_a_dropped_seq() {
        let mut timeline = dualsense();
        timeline.push(state(0, 10), 0);

        // 連番が 11 を飛ばして 12 になる。
        let dropped = timeline.push(
            with_buttons(dualsense_ticks(REPORT_INTERVAL_US), 12, Buttons::CIRCLE),
            REPORT_INTERVAL_US,
        );
        // 次のレポートは連番が繋がっているので立たない。
        let continued = timeline.push(
            with_buttons(
                dualsense_ticks(2 * REPORT_INTERVAL_US),
                13,
                Buttons::empty(),
            ),
            2 * REPORT_INTERVAL_US,
        );

        assert_eq!(dropped.len(), 1);
        assert!(dropped[0].gap_before);
        assert_eq!(continued.len(), 1);
        assert!(!continued[0].gap_before);
    }

    /// 連番は法をまたいで連続する。DualShock 4 は 63 の次が 0。
    #[test]
    fn does_not_flag_gap_when_seq_wraps() {
        for (mut timeline, last_seq) in [(dualsense(), 255u8), (dualshock4(), 63u8)] {
            timeline.push(state(0, last_seq), 0);

            let events = timeline.push(with_buttons(100, 0, Buttons::SQUARE), REPORT_INTERVAL_US);

            assert_eq!(events.len(), 1);
            assert!(!events[0].gap_before);
        }
    }

    /// 取りこぼしを検出してもイベントは作らない。補間すると存在しない F 数が出る。
    #[test]
    fn does_not_synthesize_events_for_a_gap() {
        let mut timeline = dualsense();
        timeline.push(state(0, 0), 0);

        let events = timeline.push(
            state(dualsense_ticks(REPORT_INTERVAL_US), 40),
            REPORT_INTERVAL_US,
        );

        assert!(events.is_empty());
    }

    #[test]
    fn reports_each_button_change_individually() {
        let mut timeline = dualsense();
        timeline.push(with_buttons(0, 0, Buttons::L1), 0);

        let events = timeline.push(
            with_buttons(
                dualsense_ticks(REPORT_INTERVAL_US),
                1,
                Buttons::R1 | Buttons::CROSS,
            ),
            REPORT_INTERVAL_US,
        );

        assert_eq!(events.len(), 3);
        assert!(events.contains(&InputEvent {
            kind: EventKind::Release,
            target: Target::Button(Buttons::L1),
            at_us: events[0].at_us,
            gap_before: false,
        }));
        assert!(events.contains(&InputEvent {
            kind: EventKind::Press,
            target: Target::Button(Buttons::R1),
            at_us: events[0].at_us,
            gap_before: false,
        }));
        assert!(events.contains(&InputEvent {
            kind: EventKind::Press,
            target: Target::Button(Buttons::CROSS),
            at_us: events[0].at_us,
            gap_before: false,
        }));
    }

    /// 十字キーの方向が変わったレポートは、古い方向の離しと新しい方向の押下を順に出す。
    #[test]
    fn dpad_change_releases_old_then_presses_new() {
        let mut timeline = dualsense();
        timeline.push(with_dpad(0, 0, Some(Direction::N)), 0);

        let events = timeline.push(
            with_dpad(dualsense_ticks(REPORT_INTERVAL_US), 1, Some(Direction::E)),
            REPORT_INTERVAL_US,
        );

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, EventKind::Release);
        assert_eq!(events[0].target, Target::Dpad(Direction::N));
        assert_eq!(events[1].kind, EventKind::Press);
        assert_eq!(events[1].target, Target::Dpad(Direction::E));
        assert_eq!(events[0].at_us, events[1].at_us);
    }

    #[test]
    fn stick_stays_neutral_below_deadzone() {
        let mut timeline = Timeline::new(Device::DualSense, 0.5, 15.0);
        timeline.push(state(0, 0), 0);

        let below = timeline.push(
            with_stick(dualsense_ticks(REPORT_INTERVAL_US), 1, stick_at(0.0, 0.4)),
            REPORT_INTERVAL_US,
        );
        let above = timeline.push(
            with_stick(
                dualsense_ticks(2 * REPORT_INTERVAL_US),
                2,
                stick_at(0.0, 0.6),
            ),
            2 * REPORT_INTERVAL_US,
        );
        let back = timeline.push(
            with_stick(
                dualsense_ticks(3 * REPORT_INTERVAL_US),
                3,
                stick_at(0.0, 0.4),
            ),
            3 * REPORT_INTERVAL_US,
        );

        assert!(below.is_empty());
        assert_eq!(above.len(), 1);
        assert_eq!(above[0].kind, EventKind::Press);
        assert_eq!(above[0].target, Target::Stick(Direction::E));
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].kind, EventKind::Release);
        assert_eq!(back[0].target, Target::Stick(Direction::E));
    }

    /// デッドゾーンの判定は現在の方向に依らない。倒した状態から中心に戻せば必ず中立になる。
    #[test]
    fn deadzone_ignores_hysteresis() {
        let mut timeline = Timeline::new(Device::DualSense, 0.5, 0.0);
        timeline.push(with_stick(0, 0, stick_at(90.0, 1.0)), 0);

        let events = timeline.push(
            with_stick(dualsense_ticks(REPORT_INTERVAL_US), 1, (CENTER, CENTER)),
            REPORT_INTERVAL_US,
        );

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::Release);
        assert_eq!(events[0].target, Target::Stick(Direction::N));
    }

    /// セクタ境界をまたぐ揺れを繰り返しても方向を切り替えない。
    ///
    /// 往復を 1 回だけ見ても、現在方向を境界の手前に持ち替えて少しずつずれる実装は通ってしまう。
    /// 境界の両側を往復させ、全レポートでイベントが 0 件であることを見る。
    #[test]
    fn stick_holds_direction_through_repeated_boundary_jitter() {
        // E と NE の境界は 22.5 度。ヒステリシス 15 度なので、NE の中心 45 度から 15 度以内、
        // つまり 30 度を越えるまでは E のまま。
        const INSIDE_E_DEG: f32 = 15.0;
        const INSIDE_NE_DEG: f32 = 29.0;
        const REPORTS: u64 = 20;

        let mut timeline = Timeline::new(Device::DualSense, 0.5, 15.0);
        timeline.push(with_stick(0, 0, stick_at(0.0, 1.0)), 0);

        for step in 1..=REPORTS {
            let angle_deg = if step % 2 == 0 {
                INSIDE_E_DEG
            } else {
                INSIDE_NE_DEG
            };
            let events = timeline.push(
                with_stick(
                    dualsense_ticks(step * REPORT_INTERVAL_US),
                    step as u8,
                    stick_at(angle_deg, 1.0),
                ),
                step * REPORT_INTERVAL_US,
            );

            assert!(events.is_empty(), "{step} 件目 {angle_deg} 度で {events:?}");
        }

        // 往復のあとも E を保っている。中立に戻したときに出る離しで確かめる。
        let step = REPORTS + 1;
        let release = timeline.push(
            with_stick(
                dualsense_ticks(step * REPORT_INTERVAL_US),
                step as u8,
                (CENTER, CENTER),
            ),
            step * REPORT_INTERVAL_US,
        );

        assert_eq!(release.len(), 1);
        assert_eq!(release[0].kind, EventKind::Release);
        assert_eq!(release[0].target, Target::Stick(Direction::E));
    }

    /// 候補の中心から hysteresis_deg 以内に入ったら切り替える。
    #[test]
    fn stick_switches_when_within_hysteresis_of_candidate_center() {
        let mut timeline = Timeline::new(Device::DualSense, 0.5, 15.0);
        timeline.push(with_stick(0, 0, stick_at(0.0, 1.0)), 0);

        let events = timeline.push(
            with_stick(dualsense_ticks(REPORT_INTERVAL_US), 1, stick_at(31.0, 1.0)),
            REPORT_INTERVAL_US,
        );

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, EventKind::Release);
        assert_eq!(events[0].target, Target::Stick(Direction::E));
        assert_eq!(events[1].kind, EventKind::Press);
        assert_eq!(events[1].target, Target::Stick(Direction::NE));
    }

    /// 閾値は角度で対称に効く。W と NW の境界でも E と NE と同じ幅で切り替わる。
    #[test]
    fn hysteresis_threshold_is_symmetric_across_directions() {
        for (from_deg, to_deg, from, to) in [
            (180.0f32, 149.0f32, Direction::W, Direction::NW),
            (-90.0, -59.0, Direction::S, Direction::SE),
        ] {
            let mut timeline = Timeline::new(Device::DualSense, 0.5, 15.0);
            timeline.push(with_stick(0, 0, stick_at(from_deg, 1.0)), 0);

            let events = timeline.push(
                with_stick(
                    dualsense_ticks(REPORT_INTERVAL_US),
                    1,
                    stick_at(to_deg, 1.0),
                ),
                REPORT_INTERVAL_US,
            );

            assert_eq!(events.len(), 2, "{from:?} から {to:?}");
            assert_eq!(events[0].target, Target::Stick(from));
            assert_eq!(events[1].target, Target::Stick(to));
        }
    }

    /// 状態が変わらないレポートでもイベントは出ないが時刻は進む。
    #[test]
    fn unchanged_reports_advance_now_us() {
        let mut timeline = dualsense();
        timeline.push(with_buttons(0, 0, Buttons::L1), 0);

        for step in 1..=5u64 {
            let events = timeline.push(
                with_buttons(
                    dualsense_ticks(step * REPORT_INTERVAL_US),
                    step as u8,
                    Buttons::L1,
                ),
                step * REPORT_INTERVAL_US,
            );

            assert!(events.is_empty());
            // 件数が増えても誤差が積もらない。累積を実数で持っているため。
            let expected = step * REPORT_INTERVAL_US;
            assert!(
                (timeline.now_us() as i64 - expected as i64).abs() <= 1,
                "{step} 件目で {}",
                timeline.now_us()
            );
        }
    }

    /// 時刻は単調に増える。デバイス時刻が戻ったレポートでも巻き戻さない。
    #[test]
    fn time_never_goes_backwards() {
        let mut timeline = dualsense();
        timeline.push(state(1_000_000, 0), 0);

        let mut previous_us = 0;
        for (device_ts, host_us) in [(500_000u32, 4_000u64), (0, 8_000), (2_000_000, 12_000)] {
            timeline.push(state(device_ts, 1), host_us);
            assert!(timeline.now_us() >= previous_us);
            previous_us = timeline.now_us();
        }
    }

    /// ホストの時刻が戻っても panic しない。
    #[test]
    fn tolerates_host_clock_going_backwards() {
        let mut timeline = dualsense();
        timeline.push(state(0, 0), 1_000_000);

        let events = timeline.push(with_buttons(dualsense_ticks(FRAME_US), 1, Buttons::L1), 0);

        assert_eq!(events.len(), 1);
        assert!((events[0].at_us as i64 - FRAME_US as i64).abs() <= 1);
    }

    /// 極端な値でも飽和するだけで panic しない。
    #[test]
    fn saturates_on_extreme_inputs() {
        let mut timeline = dualsense();
        timeline.push(state(0, 0), 0);
        timeline.push(state(u32::MAX, 1), u64::MAX);
        timeline.push(state(u32::MAX, 2), u64::MAX);

        assert!(timeline.now_us() > 0);
    }

    /// スティックの生値の四隅と中心が panic なく量子化される。
    #[test]
    fn quantizes_every_raw_stick_extreme() {
        let timeline = Timeline::new(Device::DualSense, 0.5, 15.0);

        for (raw, expected) in [
            ((0u8, 0u8), Some(Direction::NW)),
            ((255, 0), Some(Direction::NE)),
            ((0, 255), Some(Direction::SW)),
            ((255, 255), Some(Direction::SE)),
            ((CENTER, 0), Some(Direction::N)),
            ((CENTER, 255), Some(Direction::S)),
            ((0, CENTER), Some(Direction::W)),
            ((255, CENTER), Some(Direction::E)),
            ((CENTER, CENTER), None),
        ] {
            assert_eq!(timeline.quantize_stick(raw, None), expected, "{raw:?}");
        }
    }
}
