//! NEZIA ENGINE C ABI ラッパ。
//!
//! `core::SoundEngine` を C ABI 経由で外部言語から利用可能にする薄いラッパー。
//! 詳細は `docs/design/ffi/CONCEPT.md` 参照。
//!
//! 各 `extern "C"` 関数は共通の安全性契約を持つ:
//! - `engine` は `nezia_engine_new` の戻り値かつ未解放であること。
//! - ポインタ + 長さ引数は呼出側がその範囲の有効性を保証すること。
//! - 関数ポインタはコールバック発火時まで呼び出し可能な状態を保つこと。
//!
//! このため個別関数では `# Safety` セクションを省略している。
#![allow(clippy::missing_safety_doc)]

mod audio_meta;
mod buffer;
mod buffer_reader;
mod bus;
mod engine;
mod panic;
mod source;
mod spatial;
mod types;

// extern "C" 関数と ABI 型をクレートルートで再公開する。
// `#[unsafe(no_mangle)]` により cdylib / staticlib にはモジュール可視性に
// 関わらず symbol が出るが、Rust 側からの結合テストには名前経由のインポートが必要。
pub use audio_meta::*;
pub use buffer::*;
pub use buffer_reader::*;
pub use bus::*;
pub use engine::*;
pub use source::*;
pub use spatial::*;
pub use types::*;
