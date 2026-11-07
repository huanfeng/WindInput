//! 码表引擎
//!
//! 与 Go 版本 `wind_input/internal/engine/codetable/` 对齐。

pub mod disabled_dicts;
pub mod engine;
pub mod sentence;

pub use disabled_dicts::{DisabledDictLayers, DisabledDictSource};
pub use engine::{BaseSort, CodeTableEngine, CommitOptions, SplitAltDisplay, SplitTrigger};
pub use sentence::{CodeSentenceDecoder, SentenceResult, ShortCodeIndex};
