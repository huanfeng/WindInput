//! wind-candidate: 候选词数据类型、排序与过滤
//!
//! 与 Go 版本 `wind_input/internal/candidate/` 对齐。

pub mod candidate;
pub mod charblock;
pub mod charclass;
pub mod charemoji;
/// 生成物，只有数据；判定逻辑与取舍论证在 `charemoji`。
pub(crate) mod charemoji_data;
pub mod common;
pub mod filter;
pub mod shadow;
pub mod store;

pub use candidate::*;
pub use charblock::*;
pub use charclass::*;
pub use charemoji::*;
pub use common::*;
pub use filter::*;
pub use shadow::*;
pub use store::*;
