//! DualSense / DualShock 4 の入力をフレーム単位で並べるための構成要素。
//!
//! OS と hidapi に接する層はバイナリ側に置き、ここには実機なしでテストできるものだけを入れる。

pub mod history;
pub mod report_decode;
pub mod timeline;
