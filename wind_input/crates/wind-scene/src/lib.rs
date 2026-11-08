//! wind-scene：候选窗等浮层「纯数据 → View 树 → 像素」的平台无关部分。
//!
//! 从 wind-ui 抽出，给实时窗口、设置端预览、主题编辑器（wasm）共用。不依赖 wind-config、
//! 不碰任何平台 API；文字经 [`text::TextBackend`] 由调用方注入。方案见
//! `docs/design/wind-scene-extraction.md`。

pub mod candidate;
pub mod clock;
pub mod image_cache;
pub mod skin;
pub mod skin_anim;
pub mod span_runs;
pub mod text;
pub mod theme_assets;
pub mod view;
