//! 码表引擎
//!
//! 与 Go 版本 `wind_input/internal/engine/codetable/` 对齐。

pub mod engine;
pub mod sentence;

pub use engine::{BaseSort, CodeTableEngine, CommitOptions, SplitTrigger};
pub use sentence::{CodeSentenceDecoder, SentenceResult, ShortCodeIndex};
