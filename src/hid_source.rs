//! hidapi でデバイスを開き、ブロッキング read で生レポートを 1 件ずつ返す。受信直後に QPC を打つ。
//!
//! 書き込み系の API は呼ばない。output report と feature report を送ると Steam と取り合いになり、
//! 別のレポートのデータが返る。
//!
//! ブロッキング read はレポート到着で起床するので、ウィンドウが他のウィンドウに覆われても
//! 読みは止まらない。この前提は実機で確かめていない。
//!
//! 実装は Windows だけにある。他の OS では [`open`] が必ず失敗し、UI は未接続を出す。

use crate::report_decode::Device;

/// Sony Interactive Entertainment。
pub const SONY_VENDOR_ID: u16 = 0x054C;

/// 読み取り専用で扱う対象デバイス。`hid_probe` が列挙する PID と同じ。
pub const TARGET_PRODUCTS: [(u16, &str, Device); 4] = [
    (0x0CE6, "DualSense", Device::DualSense),
    (0x0DF2, "DualSense Edge", Device::DualSense),
    (0x05C4, "DualShock 4 (1st gen)", Device::DualShock4),
    (0x09CC, "DualShock 4 (2nd gen)", Device::DualShock4),
];

/// USB 接続の入力レポートは 64 バイトに収まる。
const REPORT_BUFFER_LEN: usize = 64;

/// 受信したレポート 1 件。
///
/// buffer は呼び出し側に持たせない。長さを間違えるとレポートが途中で切れ、
/// 切れた先にあるデバイス時刻が読めなくなる。
#[derive(Debug, Clone, Copy)]
pub struct Report {
    buffer: [u8; REPORT_BUFFER_LEN],
    len: usize,
    host_qpc_us: u64,
}

impl Report {
    /// 受信した生バイト。
    pub fn bytes(&self) -> &[u8] {
        &self.buffer[..self.len]
    }

    /// 受信直後に打った QPC のµs 値。
    pub fn host_qpc_us(&self) -> u64 {
        self.host_qpc_us
    }
}

pub use platform::{open, Connection};

#[cfg(windows)]
mod platform {
    use hidapi::{HidApi, HidDevice};
    use std::sync::OnceLock;
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

    use super::{Report, REPORT_BUFFER_LEN, SONY_VENDOR_ID, TARGET_PRODUCTS};
    use crate::report_decode::Device;

    /// `read_timeout` に渡すミリ秒。負値はレポート到着までブロックする。
    const BLOCK_UNTIL_REPORT: i32 = -1;

    /// QPC の周波数は起動中変わらない。1 度だけ読む。
    static QPC_FREQUENCY: OnceLock<i64> = OnceLock::new();

    /// 開いているデバイス。
    ///
    /// フィールドは宣言順に drop される。[`HidApi`] の drop は `hid_exit` を呼び、
    /// 開いている handle を無効にするので、`device` を先に置いて先に閉じさせる。
    pub struct Connection {
        device: HidDevice,
        /// 接続の間 [`HidApi`] を生かすためだけに持つ。
        _api: HidApi,
        kind: Device,
        name: &'static str,
    }

    /// 最初に見つかった対象デバイスを読み取り専用で開く。
    pub fn open() -> Result<Connection, String> {
        let api = HidApi::new().map_err(|err| format!("failed to initialize HidApi: {err}"))?;

        let (product_id, name, kind) = api
            .device_list()
            .filter(|info| info.vendor_id() == SONY_VENDOR_ID)
            .find_map(|info| {
                TARGET_PRODUCTS
                    .iter()
                    .find(|(product_id, _, _)| *product_id == info.product_id())
                    .copied()
            })
            .ok_or_else(|| format!("no target device found (VID {SONY_VENDOR_ID:#06X})"))?;

        let device = api
            .open(SONY_VENDOR_ID, product_id)
            .map_err(|err| format!("cannot open {name}: {err}"))?;

        Ok(Connection {
            device,
            _api: api,
            kind,
            name,
        })
    }

    impl Connection {
        /// 画面に出すデバイス名。
        pub fn name(&self) -> &'static str {
            self.name
        }

        /// レポートの構造を決めるデバイス種別。
        pub fn device(&self) -> Device {
            self.kind
        }

        /// レポートが 1 件届くまで待つ。
        pub fn read(&self) -> Result<Report, String> {
            let mut buffer = [0u8; REPORT_BUFFER_LEN];
            let len = self
                .device
                .read_timeout(&mut buffer, BLOCK_UNTIL_REPORT)
                .map_err(|err| format!("read failed on {}: {err}", self.name))?;

            Ok(Report {
                buffer,
                len,
                host_qpc_us: host_qpc_us()?,
            })
        }
    }

    /// QPC を読んでµs に直す。i128 を経由して乗算の桁溢れを避ける。
    fn host_qpc_us() -> Result<u64, String> {
        let frequency = qpc_frequency()?;

        let mut ticks = 0i64;
        // SAFETY: ticks はスタック上の有効な i64。
        unsafe { QueryPerformanceCounter(&mut ticks) }
            .map_err(|err| format!("QueryPerformanceCounter failed: {err}"))?;

        Ok((i128::from(ticks) * 1_000_000 / i128::from(frequency)) as u64)
    }

    fn qpc_frequency() -> Result<i64, String> {
        if let Some(frequency) = QPC_FREQUENCY.get() {
            return Ok(*frequency);
        }

        let mut frequency = 0i64;
        // SAFETY: frequency はスタック上の有効な i64。
        unsafe { QueryPerformanceFrequency(&mut frequency) }
            .map_err(|err| format!("QueryPerformanceFrequency failed: {err}"))?;
        if frequency <= 0 {
            return Err(format!("unusable QPC frequency: {frequency}"));
        }

        Ok(*QPC_FREQUENCY.get_or_init(|| frequency))
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Report;
    use crate::report_decode::Device;

    /// Windows 以外では [`open`] が値を返さないので、この型の値は存在しない。
    pub enum Connection {}

    pub fn open() -> Result<Connection, String> {
        Err("HID reading works on Windows only".to_owned())
    }

    impl Connection {
        pub fn name(&self) -> &'static str {
            match *self {}
        }

        pub fn device(&self) -> Device {
            match *self {}
        }

        pub fn read(&self) -> Result<Report, String> {
            match *self {}
        }
    }
}
