//! wind-store: 基于 redb 的持久化存储
//!
//! 与 Go 版本 `wind_input/internal/store/` 对齐。
//! 使用 redb 替代 bbolt，保持相同的 bucket 语义。

pub mod abbrev_index;
pub mod charsets;
pub mod common_chars;
pub mod dict_export;
pub mod freq;
pub mod import_formats;
pub mod migration;
pub mod phrase_text;
pub mod phrases;
pub mod quick_format;
pub mod shadow;
pub mod stat_collector;
pub mod stats;
pub mod store;
pub mod temp_words;
pub mod user_words;
pub mod wdict;

pub use store::Store;
