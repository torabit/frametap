//! 実行ファイルと同じディレクトリに置く `frametap.toml` の読み込み。
//!
//! 受け付けるのは数値 4 つだけで、section も配列も文字列も扱わない。汎用の TOML
//! パーサを入れずに自前で読むのは、この 4 行のためにパーサの依存を増やさないため。
//!
//! 読めなかった行と受け付けられない値は既定値のままにして、警告を [`Loaded::warnings`]
//! に積む。黙って既定値に落とすと、設定を書いたのに効いていない状態を画面から
//! 区別できない。配って使ってもらう道具なので、効かなかったことが見える側を選ぶ。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// 読み込むファイルの名前。
pub const FILE_NAME: &str = "frametap.toml";

/// F 換算の基準の既定値。
pub const DEFAULT_FPS: f64 = 60.0;

/// 試行を区切る無入力時間の既定値。
pub const DEFAULT_TRIAL_GAP_MS: u64 = 300;

/// 左スティックを方向入力と見なす閾値の既定値。
pub const DEFAULT_STICK_DEADZONE: f32 = 0.5;

/// 縦リストに積む試行数の既定値。
pub const DEFAULT_TRIALS_SHOWN: usize = 5;

/// 離した区間を行にするかの既定値。既定で出さないのは、その行が持続 F しか持たず、
/// 読む行数だけが倍になるため。間合いは押下 F の差で読める。
pub const DEFAULT_SHOW_RELEASED: bool = false;

/// 設定の値。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// F 換算の基準。
    pub fps: f64,
    /// 試行を区切る無入力時間。
    pub trial_gap_ms: u64,
    /// 左スティックを方向入力と見なす閾値。
    pub stick_deadzone: f32,
    /// 縦リストに積む試行数。
    pub trials_shown: usize,
    /// 何も押していない区間を行にするか。
    pub show_released: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fps: DEFAULT_FPS,
            trial_gap_ms: DEFAULT_TRIAL_GAP_MS,
            stick_deadzone: DEFAULT_STICK_DEADZONE,
            trials_shown: DEFAULT_TRIALS_SHOWN,
            show_released: DEFAULT_SHOW_RELEASED,
        }
    }
}

/// 読み込みの結果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Loaded {
    pub config: Config,
    /// 読めなかった行と受け付けられなかった値の説明。画面に出す。
    pub warnings: Vec<String>,
}

/// 実行ファイルと同じディレクトリの [`FILE_NAME`] を読む。
///
/// ファイルが無いときは既定値だけを返し、警告も出さない。置かずに使うのが普通の状態である。
pub fn load() -> Loaded {
    read(&file_path(executable_dir()))
}

/// 探しに行くファイルの位置。実行ファイルの場所が分からなければ現在のディレクトリを使う。
pub fn file_path(exe_dir: Option<PathBuf>) -> PathBuf {
    match exe_dir {
        Some(dir) => dir.join(FILE_NAME),
        None => PathBuf::from(FILE_NAME),
    }
}

/// 指定した位置のファイルを読む。無ければ既定値だけを返す。
pub fn read(path: &Path) -> Loaded {
    match fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Loaded::default(),
        // 置いてあるのに読めないのは権限か壊れた文字である。既定値で起動は続けるが黙らない。
        Err(err) => Loaded {
            config: Config::default(),
            warnings: vec![format!("cannot read {}: {err}", path.display())],
        },
    }
}

/// 1 行 1 キーの `key = value` を読む。
pub fn parse(text: &str) -> Loaded {
    let mut config = Config::default();
    let mut warnings = Vec::new();
    let mut seen: Vec<&str> = Vec::new();

    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }

        // section は読まない。黙って飛ばすと、中に書いたキーが効かない理由が分からない。
        if line.starts_with('[') {
            warnings.push(format!(
                "line {number}: sections are not read, ignored `{line}`"
            ));
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            warnings.push(format!("line {number}: no `=`, ignored `{line}`"));
            continue;
        };
        let (key, value) = (key.trim(), value.trim());

        if seen.contains(&key) {
            warnings.push(format!(
                "line {number}: `{key}` appears twice, using the last value"
            ));
        }
        seen.push(key);

        match key {
            "fps" => match value.parse::<f64>() {
                Ok(fps) if fps > 0.0 => config.fps = fps,
                Ok(_) => warnings.push(out_of_range(number, key, value, &DEFAULT_FPS.to_string())),
                Err(_) => warnings.push(not_a_number(number, key, value, &DEFAULT_FPS.to_string())),
            },
            "trial_gap_ms" => match value.parse::<u64>() {
                Ok(ms) => config.trial_gap_ms = ms,
                Err(_) => warnings.push(not_a_number(
                    number,
                    key,
                    value,
                    &DEFAULT_TRIAL_GAP_MS.to_string(),
                )),
            },
            "stick_deadzone" => match value.parse::<f32>() {
                Ok(deadzone) if (0.0..=1.0).contains(&deadzone) => config.stick_deadzone = deadzone,
                Ok(_) => warnings.push(out_of_range(
                    number,
                    key,
                    value,
                    &DEFAULT_STICK_DEADZONE.to_string(),
                )),
                Err(_) => warnings.push(not_a_number(
                    number,
                    key,
                    value,
                    &DEFAULT_STICK_DEADZONE.to_string(),
                )),
            },
            "trials_shown" => match value.parse::<usize>() {
                Ok(shown) if shown > 0 => config.trials_shown = shown,
                Ok(_) => warnings.push(out_of_range(
                    number,
                    key,
                    value,
                    &DEFAULT_TRIALS_SHOWN.to_string(),
                )),
                Err(_) => warnings.push(not_a_number(
                    number,
                    key,
                    value,
                    &DEFAULT_TRIALS_SHOWN.to_string(),
                )),
            },
            "show_released" => match value {
                "true" => config.show_released = true,
                "false" => config.show_released = false,
                _ => warnings.push(not_a_boolean(
                    number,
                    key,
                    value,
                    &DEFAULT_SHOW_RELEASED.to_string(),
                )),
            },
            _ => warnings.push(format!("line {number}: unknown key `{key}`")),
        }
    }

    Loaded { config, warnings }
}

/// 実行ファイルの置き場所。取れなければ `None`。
fn executable_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

/// `#` から行末までを落とす。値に文字列を取らないので、引用符の中を考えなくてよい。
fn strip_comment(line: &str) -> &str {
    match line.split_once('#') {
        Some((before, _)) => before,
        None => line,
    }
}

fn not_a_number(number: usize, key: &str, value: &str, fallback: &str) -> String {
    format!("line {number}: `{key}` = `{value}` is not a number, using default {fallback}")
}

fn not_a_boolean(number: usize, key: &str, value: &str, fallback: &str) -> String {
    format!("line {number}: `{key}` = `{value}` is not true or false, using default {fallback}")
}

fn out_of_range(number: usize, key: &str, value: &str, fallback: &str) -> String {
    format!("line {number}: `{key}` = `{value}` is out of range, using default {fallback}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_text_gives_the_defaults() {
        let loaded = parse("");

        assert_eq!(loaded.config, Config::default());
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn every_key_is_read() {
        let loaded =
            parse("fps = 30\ntrial_gap_ms = 500\nstick_deadzone = 0.25\ntrials_shown = 12\n");

        assert_eq!(loaded.config.fps, 30.0);
        assert_eq!(loaded.config.trial_gap_ms, 500);
        assert_eq!(loaded.config.stick_deadzone, 0.25);
        assert_eq!(loaded.config.trials_shown, 12);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn blank_lines_and_comments_are_skipped() {
        let loaded = parse("# 見出し\n\n   \nfps = 30\n  # 行の途中に置いた註\n");

        assert_eq!(loaded.config.fps, 30.0);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn a_comment_after_a_value_is_cut() {
        let loaded = parse("fps = 30 # 対象のゲームに合わせる\n");

        assert_eq!(loaded.config.fps, 30.0);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn an_unknown_key_keeps_the_defaults_and_warns() {
        let loaded = parse("fsp = 30\n");

        assert_eq!(loaded.config, Config::default());
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("fsp"));
    }

    #[test]
    fn a_value_that_is_not_a_number_keeps_the_default_and_warns() {
        let loaded = parse("fps = ろくじゅう\n");

        assert_eq!(loaded.config.fps, DEFAULT_FPS);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("fps"));
    }

    #[test]
    fn a_line_without_an_equals_sign_warns() {
        let loaded = parse("fps 30\n");

        assert_eq!(loaded.config, Config::default());
        assert_eq!(loaded.warnings.len(), 1);
    }

    #[test]
    fn a_section_header_warns_instead_of_being_ignored() {
        let loaded = parse("[display]\nfps = 30\n");

        assert_eq!(loaded.config.fps, 30.0);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("[display]"));
    }

    /// 0 以下の fps は 1F の長さが決まらない。
    #[test]
    fn a_non_positive_fps_keeps_the_default_and_warns() {
        for text in ["fps = 0\n", "fps = -30\n"] {
            let loaded = parse(text);

            assert_eq!(loaded.config.fps, DEFAULT_FPS, "{text}");
            assert_eq!(loaded.warnings.len(), 1, "{text}");
        }
    }

    /// 閾値は正規化した距離との比較なので 0 から 1 の外に意味が無い。
    #[test]
    fn a_stick_deadzone_outside_zero_to_one_keeps_the_default_and_warns() {
        for text in ["stick_deadzone = -0.1\n", "stick_deadzone = 1.5\n"] {
            let loaded = parse(text);

            assert_eq!(
                loaded.config.stick_deadzone, DEFAULT_STICK_DEADZONE,
                "{text}"
            );
            assert_eq!(loaded.warnings.len(), 1, "{text}");
        }
    }

    #[test]
    fn show_released_reads_true_and_false() {
        assert!(parse("show_released = true\n").config.show_released);
        assert!(!parse("show_released = false\n").config.show_released);
        assert!(parse("show_released = true\n").warnings.is_empty());
    }

    /// 既定は非表示。離した区間の行は持続 F しか持たず、読む列を増やすだけになる。
    #[test]
    fn show_released_defaults_to_false() {
        assert!(!Config::default().show_released);
    }

    #[test]
    fn a_show_released_that_is_not_a_boolean_keeps_the_default_and_warns() {
        let loaded = parse("show_released = yes\n");

        assert!(!loaded.config.show_released);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("show_released"));
    }

    /// 0 件だと画面に何も出ない。設定として通さない。
    #[test]
    fn a_zero_trials_shown_keeps_the_default_and_warns() {
        let loaded = parse("trials_shown = 0\n");

        assert_eq!(loaded.config.trials_shown, DEFAULT_TRIALS_SHOWN);
        assert_eq!(loaded.warnings.len(), 1);
    }

    /// 同じキーを 2 回書いたときは後ろが勝つ。気づかずに前の行を残した場合に備えて警告も出す。
    #[test]
    fn a_repeated_key_takes_the_last_value_and_warns() {
        let loaded = parse("fps = 30\nfps = 120\n");

        assert_eq!(loaded.config.fps, 120.0);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("fps"));
    }

    #[test]
    fn several_bad_lines_each_get_a_warning() {
        let loaded = parse("fsp = 30\nfps = はやい\ntrials_shown = 0\n");

        assert_eq!(loaded.config, Config::default());
        assert_eq!(loaded.warnings.len(), 3);
    }

    #[test]
    fn the_file_sits_next_to_the_executable() {
        let path = file_path(Some(PathBuf::from("/opt/frametap")));

        assert_eq!(path, PathBuf::from("/opt/frametap").join(FILE_NAME));
    }

    /// 実行ファイルの位置が取れなくても、探す先を決めて起動を続ける。
    #[test]
    fn without_an_executable_directory_the_file_is_looked_up_in_the_current_directory() {
        assert_eq!(file_path(None), PathBuf::from(FILE_NAME));
    }

    /// 置いていないのが普通の状態なので、無いことを警告にしない。
    #[test]
    fn a_missing_file_gives_the_defaults_without_a_warning() {
        let missing = PathBuf::from("/frametap-no-such-directory").join(FILE_NAME);

        let loaded = read(&missing);

        assert_eq!(loaded, Loaded::default());
    }
}
