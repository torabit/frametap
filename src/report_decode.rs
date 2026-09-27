//! 生の HID 入力レポートを [`PadState`] に変換する純関数。
//!
//! 扱うのは USB 接続の入力レポート (report id 0x01) だけである。Bluetooth はスコープ外で、
//! レポートの構造も CRC の有無も違うため、ここでは復号しない。
//!
//! アナログトリガとジャイロは読まない。F 単位の押下タイミングを並べるのが目的なので、
//! ボタンの on/off、左スティック、デバイス時刻、取りこぼし検出用の連番だけを取り出す。

use bitflags::bitflags;

bitflags! {
    /// 押されているボタンの集合。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Buttons: u16 {
        const SQUARE = 1 << 0;
        const CROSS = 1 << 1;
        const CIRCLE = 1 << 2;
        const TRIANGLE = 1 << 3;
        const L1 = 1 << 4;
        const R1 = 1 << 5;
        const L2 = 1 << 6;
        const R2 = 1 << 7;
        /// DualShock 4 の SHARE もこれに対応づける。
        const CREATE = 1 << 8;
        const OPTIONS = 1 << 9;
        const L3 = 1 << 10;
        const R3 = 1 << 11;
        const PS = 1 << 12;
        const TOUCHPAD = 1 << 13;
    }
}

/// 十字キーの 8 方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    NW,
}

/// 復号対象のデバイス。レポートの構造が違うので呼び出し側が指定する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    DualSense,
    DualShock4,
}

/// レポート 1 件が表すパッドの状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PadState {
    pub buttons: Buttons,
    /// 中立のときは `None`。
    pub dpad: Option<Direction>,
    /// 左スティックの (X, Y)。生値のまま返し、量子化は上位の層で行う。
    pub left_stick: (u8, u8),
    /// デバイスが打った時刻。単位はデバイスと FW によって変わるため、ここでは換算しない。
    /// DualShock 4 は 16bit の値をそのまま広げて入れる。
    pub device_ts: u32,
    /// レポートの連番。飛んだら取りこぼしである。
    pub seq: u8,
}

/// USB 接続の入力レポートの id。
const INPUT_REPORT_ID: u8 = 0x01;

/// 十字キーの値が入る下位ニブル。上位ニブルは face ボタンが使う。
const HAT_MASK: u8 = 0x0F;

/// 十字キーが中立のときの値。
const HAT_NEUTRAL: u8 = 8;

/// hat の値 0..=7 に対応する方向。北から時計回りに並ぶ。
const HAT_DIRECTIONS: [Direction; 8] = [
    Direction::N,
    Direction::NE,
    Direction::E,
    Direction::SE,
    Direction::S,
    Direction::SW,
    Direction::W,
    Direction::NW,
];

/// hat と同じバイトの上位ニブルに入る face ボタン。DualSense と DualShock 4 で共通。
const FACE_BUTTON_BITS: [(u8, Buttons); 4] = [
    (0x10, Buttons::SQUARE),
    (0x20, Buttons::CROSS),
    (0x40, Buttons::CIRCLE),
    (0x80, Buttons::TRIANGLE),
];

/// 肩ボタンからスティック押し込みまでが 1 バイトに収まる。DualSense と DualShock 4 で共通。
const SHOULDER_BUTTON_BITS: [(u8, Buttons); 8] = [
    (0x01, Buttons::L1),
    (0x02, Buttons::R1),
    (0x04, Buttons::L2),
    (0x08, Buttons::R2),
    (0x10, Buttons::CREATE),
    (0x20, Buttons::OPTIONS),
    (0x40, Buttons::L3),
    (0x80, Buttons::R3),
];

/// PS ボタンとタッチパッドクリック。DualSense と DualShock 4 で共通。
const SYSTEM_BUTTON_BITS: [(u8, Buttons); 2] = [(0x01, Buttons::PS), (0x02, Buttons::TOUCHPAD)];

/// 左スティック X。DualSense と DualShock 4 で共通。
const STICK_X_OFFSET: usize = 1;

/// 左スティック Y。
const STICK_Y_OFFSET: usize = 2;

/// DualSense の連番。1 バイトを丸ごと使う。
const DUALSENSE_SEQ_OFFSET: usize = 7;

/// DualSense の hat + face ボタン。
const DUALSENSE_HAT_OFFSET: usize = 8;

/// DualSense の肩ボタン。
const DUALSENSE_SHOULDER_OFFSET: usize = 9;

/// DualSense の PS ボタンとタッチパッド。
const DUALSENSE_SYSTEM_OFFSET: usize = 10;

/// DualSense の 32bit デバイス時刻 (LE) の先頭。
const DUALSENSE_TIMESTAMP_OFFSET: usize = 28;

/// デバイス時刻の末尾までが読めれば復号できる。以降のバイトは使わない。
const DUALSENSE_MIN_LEN: usize = DUALSENSE_TIMESTAMP_OFFSET + 4;

/// DualShock 4 の hat + face ボタン。
const DUALSHOCK4_HAT_OFFSET: usize = 5;

/// DualShock 4 の肩ボタン。
const DUALSHOCK4_SHOULDER_OFFSET: usize = 6;

/// DualShock 4 は PS ボタンとタッチパッドの 2bit に連番が同居する。
const DUALSHOCK4_SYSTEM_AND_SEQ_OFFSET: usize = 7;

/// 連番が入る上位 6bit を右端に寄せる幅。
const DUALSHOCK4_SEQ_SHIFT: u32 = 2;

/// DualShock 4 の 16bit デバイス時刻 (LE) の先頭。
const DUALSHOCK4_TIMESTAMP_OFFSET: usize = 10;

const DUALSHOCK4_MIN_LEN: usize = DUALSHOCK4_TIMESTAMP_OFFSET + 2;

/// 入力レポート 1 件を復号する。
///
/// 対象外のレポート、短すぎるレポート、十字キーに未定義の値が入ったレポートは `None` を返す。
/// 補間はしない。壊れたレポートを黙って通すと、存在しない F 数が下流に出る。
pub fn decode(device: Device, report: &[u8]) -> Option<PadState> {
    match device {
        Device::DualSense => decode_dualsense(report),
        Device::DualShock4 => decode_dualshock4(report),
    }
}

fn decode_dualsense(report: &[u8]) -> Option<PadState> {
    if report.len() < DUALSENSE_MIN_LEN || report[0] != INPUT_REPORT_ID {
        return None;
    }

    let hat_and_face = report[DUALSENSE_HAT_OFFSET];
    let timestamp: [u8; 4] = report[DUALSENSE_TIMESTAMP_OFFSET..DUALSENSE_MIN_LEN]
        .try_into()
        .ok()?;

    Some(PadState {
        buttons: face_buttons(hat_and_face)
            | shoulder_buttons(report[DUALSENSE_SHOULDER_OFFSET])
            | system_buttons(report[DUALSENSE_SYSTEM_OFFSET]),
        dpad: decode_hat(hat_and_face)?,
        left_stick: (report[STICK_X_OFFSET], report[STICK_Y_OFFSET]),
        device_ts: u32::from_le_bytes(timestamp),
        seq: report[DUALSENSE_SEQ_OFFSET],
    })
}

fn decode_dualshock4(report: &[u8]) -> Option<PadState> {
    if report.len() < DUALSHOCK4_MIN_LEN || report[0] != INPUT_REPORT_ID {
        return None;
    }

    let hat_and_face = report[DUALSHOCK4_HAT_OFFSET];
    let system_and_seq = report[DUALSHOCK4_SYSTEM_AND_SEQ_OFFSET];
    let timestamp: [u8; 2] = report[DUALSHOCK4_TIMESTAMP_OFFSET..DUALSHOCK4_MIN_LEN]
        .try_into()
        .ok()?;

    Some(PadState {
        buttons: face_buttons(hat_and_face)
            | shoulder_buttons(report[DUALSHOCK4_SHOULDER_OFFSET])
            | system_buttons(system_and_seq),
        dpad: decode_hat(hat_and_face)?,
        left_stick: (report[STICK_X_OFFSET], report[STICK_Y_OFFSET]),
        device_ts: u32::from(u16::from_le_bytes(timestamp)),
        seq: system_and_seq >> DUALSHOCK4_SEQ_SHIFT,
    })
}

/// hat と face ボタンが同居するバイトから十字キーを取り出す。
///
/// 外側の `None` は未定義の値 (9..=15) を、内側の `None` は中立を表す。
fn decode_hat(hat_and_face: u8) -> Option<Option<Direction>> {
    let hat = hat_and_face & HAT_MASK;
    if hat == HAT_NEUTRAL {
        return Some(None);
    }
    HAT_DIRECTIONS.get(usize::from(hat)).copied().map(Some)
}

fn face_buttons(hat_and_face: u8) -> Buttons {
    buttons_from_bits(hat_and_face, &FACE_BUTTON_BITS)
}

fn shoulder_buttons(bits: u8) -> Buttons {
    buttons_from_bits(bits, &SHOULDER_BUTTON_BITS)
}

fn system_buttons(bits: u8) -> Buttons {
    buttons_from_bits(bits, &SYSTEM_BUTTON_BITS)
}

fn buttons_from_bits(bits: u8, table: &[(u8, Buttons)]) -> Buttons {
    table
        .iter()
        .filter(|(mask, _)| bits & mask != 0)
        .fold(Buttons::empty(), |acc, (_, button)| acc | *button)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// USB 接続の DualSense が返すレポート長。
    const DUALSENSE_REPORT_LEN: usize = 64;

    /// USB 接続の DualShock 4 が返すレポート長。
    const DUALSHOCK4_REPORT_LEN: usize = 64;

    /// 実機で採取した先頭 32 バイト。以降は未解析なので 0 で埋めて 64 バイトにする。
    /// 未解析部を 0 にできるのは、復号がタイムスタンプ末尾 (byte 31) までしか読まないため。
    const DUALSENSE_DUMPS: [[u8; 32]; 3] = [
        [
            0x01, 0x84, 0x7F, 0x82, 0x7E, 0x00, 0x00, 0x67, 0x08, 0x00, 0x00, 0x00, 0x0A, 0xEC,
            0x3D, 0x51, 0x00, 0x00, 0x14, 0x00, 0x01, 0x00, 0x04, 0x00, 0x61, 0x1F, 0x55, 0x05,
            0x7B, 0x7B, 0xDE, 0x4B,
        ],
        [
            0x01, 0x84, 0x7F, 0x82, 0x7E, 0x00, 0x00, 0x68, 0x08, 0x00, 0x00, 0x00, 0x0B, 0xEC,
            0x3D, 0x51, 0x00, 0x00, 0x13, 0x00, 0x02, 0x00, 0xF3, 0xFF, 0x42, 0x1F, 0x63, 0x05,
            0x38, 0xAB, 0xDE, 0x4B,
        ],
        [
            0x01, 0x84, 0x7F, 0x82, 0x7E, 0x00, 0x00, 0x69, 0x08, 0x00, 0x00, 0x00, 0x0C, 0xEC,
            0x3D, 0x51, 0x00, 0x00, 0x13, 0x00, 0x03, 0x00, 0x23, 0x00, 0x35, 0x1F, 0x63, 0x05,
            0xF4, 0xDA, 0xDE, 0x4B,
        ],
    ];

    fn dualsense_report(prefix: &[u8]) -> [u8; DUALSENSE_REPORT_LEN] {
        let mut report = [0u8; DUALSENSE_REPORT_LEN];
        report[..prefix.len()].copy_from_slice(prefix);
        report
    }

    /// 中立の DualSense レポート。個々のバイトを書き換えて使う。
    fn neutral_dualsense_report() -> [u8; DUALSENSE_REPORT_LEN] {
        let mut report = dualsense_report(&DUALSENSE_DUMPS[0]);
        report[DUALSENSE_HAT_OFFSET] = HAT_NEUTRAL;
        report
    }

    /// 中立の DualShock 4 レポート。
    fn neutral_dualshock4_report() -> [u8; DUALSHOCK4_REPORT_LEN] {
        let mut report = [0u8; DUALSHOCK4_REPORT_LEN];
        report[0] = INPUT_REPORT_ID;
        report[STICK_X_OFFSET] = 0x80;
        report[STICK_Y_OFFSET] = 0x80;
        report[DUALSHOCK4_HAT_OFFSET] = HAT_NEUTRAL;
        report
    }

    #[test]
    fn decodes_real_dualsense_dumps() {
        let expected = [
            (0x4BDE_7B7Bu32, 0x67u8),
            (0x4BDE_AB38, 0x68),
            (0x4BDE_DAF4, 0x69),
        ];

        for (dump, (device_ts, seq)) in DUALSENSE_DUMPS.iter().zip(expected) {
            let state = decode(Device::DualSense, &dualsense_report(dump)).expect("復号できる");

            assert_eq!(
                state,
                PadState {
                    buttons: Buttons::empty(),
                    dpad: None,
                    left_stick: (0x84, 0x7F),
                    device_ts,
                    seq,
                }
            );
        }
    }

    /// 未解析の suffix を読んでいないことを確かめる。byte 12 以降に何が入っても結果は変わらない。
    #[test]
    fn dualsense_ignores_bytes_after_system_byte() {
        let baseline = decode(Device::DualSense, &dualsense_report(&DUALSENSE_DUMPS[0]));

        let mut report = dualsense_report(&DUALSENSE_DUMPS[0]);
        for byte in report[11..DUALSENSE_TIMESTAMP_OFFSET].iter_mut() {
            *byte = 0xFF;
        }

        assert_eq!(decode(Device::DualSense, &report), baseline);
    }

    #[test]
    fn decodes_dualsense_buttons() {
        let mut report = neutral_dualsense_report();
        report[DUALSENSE_HAT_OFFSET] = HAT_NEUTRAL | 0xF0;
        report[DUALSENSE_SHOULDER_OFFSET] = 0xFF;
        report[DUALSENSE_SYSTEM_OFFSET] = 0x03;

        let state = decode(Device::DualSense, &report).expect("復号できる");

        assert_eq!(state.buttons, Buttons::all());
        assert_eq!(state.dpad, None);
    }

    #[test]
    fn decodes_dualshock4_fixed_bytes() {
        let mut report = neutral_dualshock4_report();
        report[STICK_X_OFFSET] = 0x12;
        report[STICK_Y_OFFSET] = 0x34;
        // ↓ + ○
        report[DUALSHOCK4_HAT_OFFSET] = 4 | 0x40;
        // R1 + SHARE + R3
        report[DUALSHOCK4_SHOULDER_OFFSET] = 0x02 | 0x10 | 0x80;
        // PS + 連番 0x2A
        report[DUALSHOCK4_SYSTEM_AND_SEQ_OFFSET] = 0x01 | (0x2A << 2);
        report[DUALSHOCK4_TIMESTAMP_OFFSET] = 0x34;
        report[DUALSHOCK4_TIMESTAMP_OFFSET + 1] = 0x12;

        let state = decode(Device::DualShock4, &report).expect("復号できる");

        assert_eq!(
            state,
            PadState {
                buttons: Buttons::CIRCLE
                    | Buttons::R1
                    | Buttons::CREATE
                    | Buttons::R3
                    | Buttons::PS,
                dpad: Some(Direction::S),
                left_stick: (0x12, 0x34),
                device_ts: 0x1234,
                seq: 0x2A,
            }
        );
    }

    /// wrong warp の練習で読みたいのは L1 と R1 の関係なので、同時押しが両方立つことを確かめる。
    #[test]
    fn decodes_simultaneous_shoulder_buttons() {
        let mut dualsense = neutral_dualsense_report();
        dualsense[DUALSENSE_SHOULDER_OFFSET] = 0x01 | 0x02;

        let mut dualshock4 = neutral_dualshock4_report();
        dualshock4[DUALSHOCK4_SHOULDER_OFFSET] = 0x01 | 0x02;

        for (device, report) in [
            (Device::DualSense, dualsense.as_slice()),
            (Device::DualShock4, dualshock4.as_slice()),
        ] {
            let state = decode(device, report).expect("復号できる");
            assert_eq!(state.buttons, Buttons::L1 | Buttons::R1, "{device:?}");
        }
    }

    #[test]
    fn decodes_all_hat_values() {
        let expected = [
            (0, Some(Direction::N)),
            (1, Some(Direction::NE)),
            (2, Some(Direction::E)),
            (3, Some(Direction::SE)),
            (4, Some(Direction::S)),
            (5, Some(Direction::SW)),
            (6, Some(Direction::W)),
            (7, Some(Direction::NW)),
            (8, None),
        ];

        for (hat, dpad) in expected {
            let mut dualsense = neutral_dualsense_report();
            dualsense[DUALSENSE_HAT_OFFSET] = hat;

            let mut dualshock4 = neutral_dualshock4_report();
            dualshock4[DUALSHOCK4_HAT_OFFSET] = hat;

            for (device, report) in [
                (Device::DualSense, dualsense.as_slice()),
                (Device::DualShock4, dualshock4.as_slice()),
            ] {
                let state = decode(device, report).expect("復号できる");
                assert_eq!(state.dpad, dpad, "{device:?} hat {hat}");
            }
        }
    }

    #[test]
    fn rejects_undefined_hat_values() {
        for hat in 9..=15u8 {
            let mut dualsense = neutral_dualsense_report();
            dualsense[DUALSENSE_HAT_OFFSET] = hat;

            let mut dualshock4 = neutral_dualshock4_report();
            dualshock4[DUALSHOCK4_HAT_OFFSET] = hat;

            for (device, report) in [
                (Device::DualSense, dualsense.as_slice()),
                (Device::DualShock4, dualshock4.as_slice()),
            ] {
                assert_eq!(decode(device, report), None, "{device:?} hat {hat}");
            }
        }
    }

    #[test]
    fn rejects_short_reports() {
        let dualsense = neutral_dualsense_report();
        let dualshock4 = neutral_dualshock4_report();

        for (device, report, min_len) in [
            (Device::DualSense, dualsense.as_slice(), DUALSENSE_MIN_LEN),
            (
                Device::DualShock4,
                dualshock4.as_slice(),
                DUALSHOCK4_MIN_LEN,
            ),
        ] {
            assert_eq!(decode(device, &report[..min_len - 1]), None, "{device:?}");
            assert_eq!(decode(device, &[]), None, "{device:?}");
            assert!(decode(device, &report[..min_len]).is_some(), "{device:?}");
        }
    }

    #[test]
    fn rejects_other_report_ids() {
        let mut dualsense = neutral_dualsense_report();
        dualsense[0] = 0x31;

        let mut dualshock4 = neutral_dualshock4_report();
        dualshock4[0] = 0x11;

        for (device, report) in [
            (Device::DualSense, dualsense.as_slice()),
            (Device::DualShock4, dualshock4.as_slice()),
        ] {
            assert_eq!(decode(device, report), None, "{device:?}");
        }
    }
}
