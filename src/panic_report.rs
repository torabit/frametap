//! panic の内容をファイルに残す。
//!
//! release build では [`main`] に `windows_subsystem = "windows"` が付いて console が
//! 無くなるので、既定の hook が書いた内容はどこにも出ない。実機で動かすのは Windows と
//! DualSense を持つ人なので、落ちた理由を手元に残せないと「落ちた」以外を返せない。
//!
//! 追記にするのは、再現の回数と間隔も判断の材料になるため。上書きすると最後の 1 回しか残らない。
//!
//! [`main`]: https://doc.rust-lang.org/reference/runtime.html

use std::backtrace::Backtrace;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::panic;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 書き出すファイルの名前。
pub const FILE_NAME: &str = "frametap-panic.txt";

/// 位置が取れなかったときに書く文字。
pub const UNKNOWN_LOCATION: &str = "位置不明";

/// panic の hook を差し込む。既定の hook は残す。
///
/// 既定の hook を外さないのは、debug build では console が生きているため。開発中に
/// panic が標準エラー出力から消えると、ファイルを開き直す手間が毎回かかる。
pub fn install() {
    let previous = panic::take_hook();
    let primary = file_path(executable_dir());
    let fallback = file_path(None);

    panic::set_hook(Box::new(move |info| {
        let report = format_report(
            secs_since_epoch(),
            info.payload_as_str().unwrap_or("内容不明"),
            info.location().map(ToString::to_string).as_deref(),
            &Backtrace::force_capture().to_string(),
        );

        // 実行ファイルの隣が書けないことがある。Program Files の下に置かれた場合や、
        // 読み取り専用の媒体から起動した場合になる。書けないなら一時ディレクトリに回す。
        if append(&primary, &report).is_err() {
            let _ = append(&fallback, &report);
        }

        previous(info);
    }));
}

/// 書き出す先。実行ファイルの隣を使い、そこが分からなければ一時ディレクトリに置く。
pub fn file_path(exe_dir: Option<PathBuf>) -> PathBuf {
    match exe_dir {
        Some(dir) => dir.join(FILE_NAME),
        None => std::env::temp_dir().join(FILE_NAME),
    }
}

/// ファイルに 1 件追記する。
pub fn append(path: &Path, body: &str) -> io::Result<()> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(body.as_bytes())
}

/// 書き出す 1 件分の文字列。`secs_since_epoch` は起きた時刻。
pub fn format_report(
    secs_since_epoch: u64,
    message: &str,
    location: Option<&str>,
    backtrace: &str,
) -> String {
    format!(
        "---- panic ----\n\
         時刻: {secs_since_epoch} (UNIX epoch 秒)\n\
         位置: {}\n\
         内容: {message}\n\
         backtrace:\n{backtrace}\n\n",
        location.unwrap_or(UNKNOWN_LOCATION)
    )
}

/// 実行ファイルの置き場所。取れなければ `None`。
fn executable_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

/// 時刻を epoch 秒で読む。書式を整えるために依存を増やさない。並べ替えと間隔だけが要る。
fn secs_since_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_carries_the_message_the_location_and_the_backtrace() {
        let report = format_report(
            1_774_000_000,
            "読めない値",
            Some("src/app.rs:42:9"),
            "0: frametap::main",
        );

        assert!(report.contains("読めない値"), "{report}");
        assert!(report.contains("src/app.rs:42:9"), "{report}");
        assert!(report.contains("0: frametap::main"), "{report}");
    }

    /// 追記なので、どの行がいつの panic かを時刻で分ける。回数と間隔も判断の材料になる。
    #[test]
    fn a_report_carries_the_time_it_happened() {
        let report = format_report(1_774_000_000, "読めない値", None, "");

        assert!(report.contains("1774000000"), "{report}");
    }

    /// 位置が取れないのは panic の作り方によるので、報告を落とさず穴だけを示す。
    #[test]
    fn a_report_without_a_location_says_so() {
        let report = format_report(0, "読めない値", None, "");

        assert!(report.contains(UNKNOWN_LOCATION), "{report}");
    }

    #[test]
    fn the_file_sits_next_to_the_executable() {
        let path = file_path(Some(PathBuf::from("/opt/frametap")));

        assert_eq!(path, PathBuf::from("/opt/frametap").join(FILE_NAME));
    }

    /// 実行ファイルの位置が取れなくても書ける先を返す。ここで諦めると報告が残らない。
    #[test]
    fn without_an_executable_directory_the_file_goes_to_the_temporary_directory() {
        assert_eq!(file_path(None), std::env::temp_dir().join(FILE_NAME));
    }

    /// 2 回落ちたら 2 件残る。上書きすると再現の間隔が分からない。
    #[test]
    fn a_second_report_is_appended() {
        let path = std::env::temp_dir().join(format!("frametap-append-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);

        append(&path, "1 回目").expect("1 回目を書けない");
        append(&path, "2 回目").expect("2 回目を書けない");

        let written = std::fs::read_to_string(&path).expect("読めない");
        std::fs::remove_file(&path).expect("消せない");

        assert!(written.contains("1 回目"), "{written}");
        assert!(written.contains("2 回目"), "{written}");
    }

    /// 書けない先だったことを呼び出し側が知れる。panic の中なので、ここから先は諦める。
    #[test]
    fn appending_to_an_unreachable_path_fails() {
        let unreachable = PathBuf::from("/frametap-no-such-directory").join(FILE_NAME);

        assert!(append(&unreachable, "本文").is_err());
    }
}
