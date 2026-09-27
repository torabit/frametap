//! DualSense / DualShock 4 の入力をフレーム単位で並べるための構成要素。
//!
//! データは `hid_source` → `report_decode` → `timeline` → `history` → `ui` の一方向に流れ、
//! スレッドの配線だけを `app` が持つ。OS と hidapi に接するのは `hid_source` だけで、
//! 他のモジュールは実機なしでテストできる。

pub mod app;
pub mod config;
pub mod hid_source;
pub mod history;
pub mod panic_report;
pub mod report_decode;
pub mod timeline;
pub mod ui;
