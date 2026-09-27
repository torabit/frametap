//! DualSense / DualShock 4 の HID を読み取り専用で開けるかを実機で確かめる調査用バイナリ。
//!
//! 設計 spec の「未検証の前提」のうち、Steam 起動中に物理 HID を開けること、
//! ブロッキング read でレポートが届くこと、レポート間隔が 4ms 付近に収まることを観測する。
//! output report と feature report は一切送らない。Steam と feature report を取り合うと
//! 別のレポートのデータが返るため、書き込み系の API はここでは呼ばない。

#[cfg(windows)]
mod probe {
    use hidapi::{DeviceInfo, HidApi, HidDevice};
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

    /// Sony Interactive Entertainment。
    const SONY_VENDOR_ID: u16 = 0x054C;

    /// 読み取り専用で扱う対象デバイス。
    const TARGET_PRODUCTS: [(u16, &str); 4] = [
        (0x0CE6, "DualSense"),
        (0x0DF2, "DualSense Edge"),
        (0x05C4, "DualShock 4 (1st gen)"),
        (0x09CC, "DualShock 4 (2nd gen)"),
    ];

    /// USB 接続の入力レポートは 64 バイトに収まる。
    const REPORT_BUFFER_LEN: usize = 64;

    /// 間隔の統計を取るために読むレポート数。250Hz なら約 1.2 秒。
    const REPORTS_TO_READ: usize = 300;

    /// 先頭何件をバイト列として出すか。
    const DUMPED_REPORT_COUNT: usize = 3;

    /// 1 件あたり何バイトまで出すか。
    const DUMPED_BYTE_COUNT: usize = 32;

    /// `read_timeout` に渡すミリ秒。負値はレポート到着までブロックする。
    const BLOCK_UNTIL_REPORT: i32 = -1;

    pub fn run() -> Result<(), String> {
        let api = HidApi::new().map_err(|err| format!("HidApi の初期化に失敗した: {err}"))?;

        let sony_devices: Vec<&DeviceInfo> = api
            .device_list()
            .filter(|info| info.vendor_id() == SONY_VENDOR_ID)
            .collect();

        print_sony_devices(&sony_devices);

        let target = sony_devices
            .iter()
            .find(|info| product_name(info.product_id()).is_some())
            .ok_or_else(|| {
                format!("対象デバイスが見つからない (VID {SONY_VENDOR_ID:#06X} の既知 PID なし)")
            })?;

        println!(
            "\n開く: {} (VID {:#06X} PID {:#06X})",
            product_name(target.product_id()).unwrap_or("unknown"),
            target.vendor_id(),
            target.product_id(),
        );

        // VID/PID に一致する最初のデバイスを開く。同一 VID/PID が複数並ぶ場合にどれが開くかは
        // hidapi の列挙順に従う。上の一覧と突き合わせて確かめる。
        let device = api
            .open(target.vendor_id(), target.product_id())
            .map_err(|err| err.to_string())?;

        let reception_ticks = read_reports(&device)?;
        print_interval_stats(&reception_ticks)?;

        Ok(())
    }

    fn print_sony_devices(devices: &[&DeviceInfo]) {
        if devices.is_empty() {
            println!("Sony (VID {SONY_VENDOR_ID:#06X}) のデバイスは見つからなかった");
            return;
        }

        println!(
            "Sony (VID {SONY_VENDOR_ID:#06X}) のデバイス {} 件",
            devices.len()
        );
        for info in devices {
            let identified = match product_name(info.product_id()) {
                Some(name) => name,
                None => "未知の PID",
            };
            println!(
                "  PID {:#06X} [{}] usage_page={:#06X} usage={:#06X} interface={} product={:?} manufacturer={:?}",
                info.product_id(),
                identified,
                info.usage_page(),
                info.usage(),
                info.interface_number(),
                info.product_string().unwrap_or("-"),
                info.manufacturer_string().unwrap_or("-"),
            );
            println!("    path={}", info.path().to_string_lossy());
        }
    }

    fn product_name(product_id: u16) -> Option<&'static str> {
        TARGET_PRODUCTS
            .iter()
            .find(|(pid, _)| *pid == product_id)
            .map(|(_, name)| *name)
    }

    /// `read_timeout` をちょうど [`REPORTS_TO_READ`] 回呼び、各回の直後に QPC を読む。
    /// 戻り値は受信時刻の tick 列で、長さは呼んだ回数と一致する。
    fn read_reports(device: &HidDevice) -> Result<Vec<i64>, String> {
        let mut buf = [0u8; REPORT_BUFFER_LEN];
        let mut reception_ticks = Vec::with_capacity(REPORTS_TO_READ);

        println!("\n{REPORTS_TO_READ} 件のレポートを待つ");
        println!(
            "先頭 {DUMPED_REPORT_COUNT} 件は受信した時点で出す (最大 {DUMPED_BYTE_COUNT} バイト)"
        );

        for index in 0..REPORTS_TO_READ {
            let len = device
                .read_timeout(&mut buf, BLOCK_UNTIL_REPORT)
                .map_err(|err| format!("read_timeout に失敗した ({} 件目): {err}", index + 1))?;
            reception_ticks.push(performance_counter()?);

            // 途中で読みが止まっても先頭の中身は残るよう、統計を待たずにここで出す。
            if index < DUMPED_REPORT_COUNT {
                dump_report(index, &buf[..len]);
            }
        }

        Ok(reception_ticks)
    }

    fn dump_report(index: usize, bytes: &[u8]) {
        let shown = bytes.len().min(DUMPED_BYTE_COUNT);
        let hex: Vec<String> = bytes[..shown]
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect();
        println!("  #{} len={} : {}", index + 1, bytes.len(), hex.join(" "));
    }

    /// 隣り合う受信時刻の差を取る。[`REPORTS_TO_READ`] 件なら区間は 1 つ少ない。
    fn print_interval_stats(reception_ticks: &[i64]) -> Result<(), String> {
        let frequency = performance_frequency()?;

        let mut intervals_us: Vec<i64> = reception_ticks
            .windows(2)
            .map(|pair| ticks_to_micros(pair[1] - pair[0], frequency))
            .collect();

        if intervals_us.is_empty() {
            println!("\nレポートが 2 件未満のため間隔を出せない");
            return Ok(());
        }

        intervals_us.sort_unstable();

        println!(
            "\n受信間隔 ({} 区間, QPC frequency {} Hz)",
            intervals_us.len(),
            frequency
        );
        println!("  min    {:>8} µs", intervals_us[0]);
        println!("  median {:>8} µs", median(&intervals_us));
        println!("  max    {:>8} µs", intervals_us[intervals_us.len() - 1]);

        Ok(())
    }

    /// 昇順に並んだ列の中央値。偶数個なら中央 2 つの平均を取る。
    fn median(sorted_us: &[i64]) -> i64 {
        let len = sorted_us.len();
        if len % 2 == 1 {
            sorted_us[len / 2]
        } else {
            (sorted_us[len / 2 - 1] + sorted_us[len / 2]) / 2
        }
    }

    /// QPC の tick 差をµs に直す。i128 を経由して乗算の桁溢れを避ける。
    fn ticks_to_micros(ticks: i64, frequency: i64) -> i64 {
        (i128::from(ticks) * 1_000_000 / i128::from(frequency)) as i64
    }

    fn performance_counter() -> Result<i64, String> {
        let mut ticks = 0i64;
        // SAFETY: ticks はスタック上の有効な i64。
        unsafe { QueryPerformanceCounter(&mut ticks) }
            .map_err(|err| format!("QueryPerformanceCounter に失敗した: {err}"))?;
        Ok(ticks)
    }

    fn performance_frequency() -> Result<i64, String> {
        let mut frequency = 0i64;
        // SAFETY: frequency はスタック上の有効な i64。
        unsafe { QueryPerformanceFrequency(&mut frequency) }
            .map_err(|err| format!("QueryPerformanceFrequency に失敗した: {err}"))?;
        Ok(frequency)
    }
}

#[cfg(windows)]
fn main() {
    if let Err(message) = probe::run() {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {}
