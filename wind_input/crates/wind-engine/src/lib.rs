//! wind-engine: 输入引擎（拼音、码表、混合）
//!
//! 与 Go 版本 `wind_input/internal/engine/` 对齐。

pub mod active_hook;
pub mod charset_assembly;
pub mod codetable;
pub mod encoder;
pub mod engine;
pub mod english;
pub mod freq_rerank;
pub mod manager;
pub mod mixed;
pub mod pinyin;

pub use codetable::CodeTableEngine;
pub use engine::{
    AdmitFn, BoundaryResolution, ConvertOptions, ConvertResult, Engine, EngineType, ExtendedEngine,
};
pub use english::EnglishEngine;
pub use manager::{EngineManager, FreqSettings, FreqStrategy};
pub use pinyin::PinyinEngine;
