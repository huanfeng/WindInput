//! 主题编辑器的候选窗预览：把 `wind-scene` 编成 wasm，像素与实机同一条排版与绘制。
//!
//! 文字回调浏览器 Canvas（[`canvas`]，JS 侧在 `js/text.js`）；主题经 `wind_theme` 的文本入口
//! 解析，base 链里的内置主题编进产物（[`builtin`]）；图片走 `wind_scene::assets` 的内存资源表。
//! API 本体在 [`preview`]（原生可测），wasm-bindgen 导出层在 `web`（仅 wasm32）；JS 侧推荐入口是
//! `js/preview.js` 的 `AutoPreviewer`（出错自动重建）。
//! 设计见 `docs/design/theme-editor-wasm.md`。

pub mod builtin;
pub mod canvas;
pub mod preview;
#[cfg(target_arch = "wasm32")]
mod web;
