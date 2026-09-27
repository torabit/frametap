//! HID を読み取り専用で開けるかを実機で確かめる調査用バイナリ。
//!
//! 設計 spec の「未検証の前提」のうち、Steam 起動中に物理 HID を開けること、
//! ブロッキング read でレポートが届くこと、レポート間隔が 4ms 付近に収まることを観測する。
//! output report と feature report は一切送らない。Steam と feature report を取り合うと
//! 別のレポートのデータが返るため、書き込み系の API はここでは呼ばない。
//!
//! 引数なしなら Sony の既知の機種を探す。`--list` は繋がっている HID を全部並べ、
//! `--vid` と `--pid` は指定した 1 台を開く。対応していない機種のレポートを読むために要る。

mod args {
    //! 引数の解析。[`probe`] の外に置くのは、Windows 以外でも test を走らせるため。

    /// 引数を付けないときに探す機種の説明。
    pub const USAGE: &str = "\
使い方:
  hid_probe                            Sony の既知の機種を探して 1 台開く
  hid_probe --list                     繋がっている HID を全部並べる (開かない)
  hid_probe --vid <16進> --pid <16進>  指定した 1 台を開く

例:
  hid_probe --vid 0x0F0D --pid 0x0084";

    /// 何をするか。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Command {
        /// Sony の既知の機種を探して開く。
        KnownDevice,
        /// 繋がっている HID を並べるだけ。
        List,
        /// 指定した 1 台を開く。
        Open { vendor_id: u16, product_id: u16 },
    }

    /// `argv[0]` を除いた引数を読む。
    pub fn parse(args: &[String]) -> Result<Command, String> {
        let mut vendor_id = None;
        let mut product_id = None;
        let mut list = false;
        let mut rest = args.iter();

        while let Some(arg) = rest.next() {
            match arg.as_str() {
                "--list" => list = true,
                "--vid" => vendor_id = Some(hex(&mut rest, "--vid")?),
                "--pid" => product_id = Some(hex(&mut rest, "--pid")?),
                other => return Err(format!("知らない引数: {other}")),
            }
        }

        match (list, vendor_id, product_id) {
            (true, None, None) => Ok(Command::List),
            // --list と VID/PID を同時に受けると、並べたのか開いたのかが出力から読めなくなる。
            (true, _, _) => Err("--list と --vid や --pid は一緒に使えない".to_owned()),
            (false, None, None) => Ok(Command::KnownDevice),
            (false, Some(vendor_id), Some(product_id)) => Ok(Command::Open {
                vendor_id,
                product_id,
            }),
            // 片方だけを受けると、同じ VID の別のデバイスを開いたときに何を読んだのか分からない。
            (false, Some(_), None) => Err("--vid には --pid も要る".to_owned()),
            (false, None, Some(_)) => Err("--pid には --vid も要る".to_owned()),
        }
    }

    /// 次の引数を 16 進として読む。`0x` は付いていても付いていなくてもよい。
    fn hex<'a>(rest: &mut impl Iterator<Item = &'a String>, name: &str) -> Result<u16, String> {
        let value = rest.next().ok_or_else(|| format!("{name} に値が無い"))?;
        let digits = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
            .unwrap_or(value);

        u16::from_str_radix(digits, 16)
            .map_err(|_| format!("{name} の {value} を 16 進として読めない"))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn parse_args(args: &[&str]) -> Result<Command, String> {
            let owned: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
            parse(&owned)
        }

        #[test]
        fn no_argument_looks_for_the_known_devices() {
            assert_eq!(parse_args(&[]), Ok(Command::KnownDevice));
        }

        #[test]
        fn list_only_lists() {
            assert_eq!(parse_args(&["--list"]), Ok(Command::List));
        }

        #[test]
        fn a_vid_and_a_pid_open_that_device() {
            let expected = Ok(Command::Open {
                vendor_id: 0x0F0D,
                product_id: 0x0084,
            });

            assert_eq!(
                parse_args(&["--vid", "0x0F0D", "--pid", "0x0084"]),
                expected
            );
            // 0x は省ける。大文字と小文字も区別しない。
            assert_eq!(parse_args(&["--vid", "0f0d", "--pid", "84"]), expected);
            // 順番は問わない。
            assert_eq!(
                parse_args(&["--pid", "0x0084", "--vid", "0x0F0D"]),
                expected
            );
        }

        #[test]
        fn one_of_the_two_is_refused() {
            assert!(parse_args(&["--vid", "0x0F0D"]).is_err());
            assert!(parse_args(&["--pid", "0x0084"]).is_err());
        }

        #[test]
        fn list_does_not_combine_with_an_address() {
            assert!(parse_args(&["--list", "--vid", "0x0F0D", "--pid", "0x0084"]).is_err());
        }

        #[test]
        fn a_value_that_is_not_hexadecimal_is_refused() {
            let err = parse_args(&["--vid", "ZZZZ", "--pid", "0x0084"]).unwrap_err();

            assert!(err.contains("ZZZZ"), "{err}");
        }

        /// 16 進で 4 桁を超える値は u16 に入らない。
        #[test]
        fn a_value_wider_than_u16_is_refused() {
            assert!(parse_args(&["--vid", "0x10F0D", "--pid", "0x0084"]).is_err());
        }

        #[test]
        fn a_missing_value_is_refused() {
            let err = parse_args(&["--vid"]).unwrap_err();

            assert!(err.contains("--vid"), "{err}");
        }

        #[test]
        fn an_unknown_argument_is_refused() {
            let err = parse_args(&["--all"]).unwrap_err();

            assert!(err.contains("--all"), "{err}");
        }
    }
}

#[cfg(windows)]
mod probe {
    use hidapi::{DeviceInfo, HidApi, HidDevice};

    use crate::args::Command;
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

    pub fn run(command: Command) -> Result<(), String> {
        let api = HidApi::new().map_err(|err| format!("HidApi の初期化に失敗した: {err}"))?;

        match command {
            Command::List => {
                list_devices(&api);
                Ok(())
            }
            Command::KnownDevice => {
                let sony_devices: Vec<&DeviceInfo> = api
                    .device_list()
                    .filter(|info| info.vendor_id() == SONY_VENDOR_ID)
                    .collect();

                print_devices(&format!("Sony (VID {SONY_VENDOR_ID:#06X})"), &sony_devices);

                let target = sony_devices
                    .iter()
                    .find(|info| product_name(info.product_id()).is_some())
                    .ok_or_else(|| {
                        format!("対象デバイスが見つからない (VID {SONY_VENDOR_ID:#06X} の既知 PID なし)")
                    })?;

                open_and_read(&api, target.vendor_id(), target.product_id())
            }
            Command::Open {
                vendor_id,
                product_id,
            } => open_and_read(&api, vendor_id, product_id),
        }
    }

    /// 繋がっている HID を全部並べる。開かない。開くと他のアプリと device を取り合う。
    fn list_devices(api: &HidApi) {
        let devices: Vec<&DeviceInfo> = api.device_list().collect();
        print_devices("繋がっている HID", &devices);
    }

    /// VID/PID に一致する最初のデバイスを開いて読む。同一 VID/PID が複数並ぶ場合に
    /// どれが開くかは hidapi の列挙順に従う。一覧と突き合わせて確かめる。
    fn open_and_read(api: &HidApi, vendor_id: u16, product_id: u16) -> Result<(), String> {
        println!(
            "\n開く: {} (VID {vendor_id:#06X} PID {product_id:#06X})",
            product_name(product_id).unwrap_or("未知の PID"),
        );

        let device = api.open(vendor_id, product_id).map_err(|err| {
            format!("VID {vendor_id:#06X} PID {product_id:#06X} を開けない: {err}")
        })?;

        let reception_ticks = read_reports(&device)?;
        print_interval_stats(&reception_ticks)
    }

    fn print_devices(label: &str, devices: &[&DeviceInfo]) {
        if devices.is_empty() {
            println!("{label} のデバイスは見つからなかった");
            return;
        }

        println!("{label} のデバイス {} 件", devices.len());
        for info in devices {
            let identified = product_name(info.product_id()).unwrap_or("未知の PID");
            println!(
                "  VID {:#06X} PID {:#06X} [{}] usage_page={:#06X} usage={:#06X} interface={} product={:?} manufacturer={:?}",
                info.vendor_id(),
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

/// 引数を読む。読めなければ使い方を出して終了コード 2 で終わる。
/// 1 は実行時の失敗に取ってあるので、指定の誤りと読み分けられるようにする。
fn command() -> args::Command {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    match args::parse(&argv) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("{message}\n\n{}", args::USAGE);
            std::process::exit(2);
        }
    }
}

#[cfg(windows)]
fn main() {
    if let Err(message) = probe::run(command()) {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    // 引数の誤りは Windows 以外でも同じように断る。HID の読み取りだけが Windows に依る。
    let _ = command();
    eprintln!("HID の読み取りは Windows でのみ動く");
    std::process::exit(1);
}
