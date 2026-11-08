<!-- Parent: ../../AGENTS.md -->
<!-- Updated: 2026-11-08 -->

# wind-scene-wasm

## Purpose
主题编辑器（`WindInputThemeEditor`）的候选窗预览：把 `wind-scene` 编成 wasm，像素与实机同一条排版与绘制
（`effective_theme` → `render_candidate`）。设计见 `docs/design/theme-editor-wasm.md`。

## Key Files
| File | Description |
|------|-------------|
| `src/preview.rs` | API 本体 `Preview<C>`：`set_theme(toml, 资源)` / `render(RenderOpts)` / `signal(事件)`，原生可测 |
| `src/canvas.rs` | `Canvas` trait（宿主的测量 / 画字）+ `CanvasText`（实现 `TextBackend`，直通 RGBA → 预乘 BGRA 合成）；测试用 `mock::MockCanvas` |
| `src/builtin.rs` | 编进产物的 `data/themes/` 内置主题与图片资产 |
| `src/web.rs` | wasm-bindgen 导出层（仅 wasm32）：`Previewer` / `Frame`，JS 画布接 `js/text.js` |
| `js/text.js` | OffscreenCanvas 文字后端，随产物出（wasm-bindgen 的 snippets/） |
| `js/smoke.mjs` | node 冒烟（假 OffscreenCanvas），`build-scene-wasm.sh` 有 node 时自动跑 |

## For AI Agents
- **只准依赖 wind-scene / wind-theme / wind-ui-types**（wind-config 带 uuid / dirs 挡 wasm）；wasm-bindgen / js-sys 只在 wasm32 目标上依赖。
- 新增 / 删除内置主题或其图片要同步 `src/builtin.rs`，`lists_every_builtin_theme_and_asset` 守着。
- 示例候选与序号兜底取 `wind_ui_types::preview_candidates` / `theme_index_label`，与设置端预览同源，别在这里另写一套。
- 构建：`scripts/build-scene-wasm.sh [输出目录]`（profile `wasm-release`）；wasm-bindgen CLI 版本须等于 `Cargo.lock` 里的 wasm-bindgen。检查：`scripts/dev.sh wk`。
