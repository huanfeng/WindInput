//! wind-engine: 输入引擎（拼音、码表、混合）
//!
//! 与 Go 版本 `wind_input/internal/engine/` 对齐。

pub mod active_hook;
pub mod charset_assembly;
pub mod codetable;
pub mod encoder;
pub mod engine;
pub mod english;
pub mod english_merge;
pub mod english_phrase;
pub mod freq_rerank;
pub mod manager;
pub mod mixed;
pub mod pinyin;
pub mod text_codes;
pub mod user_assoc;

pub use codetable::CodeTableEngine;
pub use engine::{
    AdmitFn, BoundaryResolution, ConvertOptions, ConvertResult, Engine, EngineType, ExtendedEngine,
    MemPart,
};
pub use english::EnglishEngine;
pub use manager::{
    ActiveDataFacts, AuxCodeSettings, AuxCodeSourceOptions, AuxSource, EngineManager, FreqSettings,
    FreqStrategy, MemoryReport, SchemaDataFacts, SchemaDictFile,
};
pub use pinyin::PinyinEngine;
pub use text_codes::TextCodeView;
