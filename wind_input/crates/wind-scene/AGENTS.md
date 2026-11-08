<!-- Parent: ../../AGENTS.md -->
<!-- Updated: 2026-11-07 -->

# wind-scene

## Purpose
候选窗等浮层渲染的平台无关部分：View 盒模型、图片缓存、异形皮肤、文字公共类型与 `TextBackend` trait。
从 wind-ui 逐步抽出（P3，方案见 `docs/design/wind-scene-extraction.md`），wind-ui 以 `pub use` 保留原路径。

## Key Files
| File | Description |
|------|-------------|
| `src/candidate/mod.rs` | 候选窗场景：`CandidateSceneInput`（每帧纯数据）→ `CandidateScene::layout` / `relayout_above` / `hits` / `paint` |
| `src/candidate/skin_geom.rs` | 候选窗的皮肤几何：求值、外扩布局、分槽位、绘制、避让与对齐点；动画时钟 `AnimClock` 由调用方注入 |
| `src/candidate/render.rs` | 一次出图 `render_candidate`（调用方持有的场景 + 输入 + 时钟 + 画不画软件阴影 → 预乘 BGRA 帧，`to_rgba` 转直通 RGBA），设置端预览 / 编辑器用 |
| `src/candidate/theme.rs` | 生效主题 `effective_theme`（原始主题 + 外观覆盖 `ThemeOverrides`；字体名解析由调用方闭包注入），实时窗口与设置端共用 |
| `src/text/mod.rs` | `TextMetrics` / `TextStyle` / `ColorRun` / `utf16_runs` / `measure_key` + `TextBackend` trait、钉基准字号的 `SizedText`；测试用 `MockText` |
| `src/text/script.rs` | 按脚本切字体段（`FontPlan` / `font_runs` / `upright_cells`） |
| `src/span_runs.rs` | 带样式文字 → 绘制用颜色区间 |
| `src/view.rs` | View 盒模型引擎 + 线程局部的图片 / 阴影缓存（闲置回收由 wind-ui 的 UI 循环登记） |
| `src/image_cache.rs` | 背景图 / 皮肤 / 动画帧解码与合成缓存 |
| `src/skin.rs` / `src/skin_anim.rs` | 异形皮肤轴映射与合成、装饰动画状态机 |
| `src/theme_assets.rs` | `RvImage` → `ViewImage` / `ViewLayer` |
| `src/clock.rs` | 取时间垫片 `now()` |

## For AI Agents
- **不准依赖 wind-config**（uuid / dirs 挡 wasm）；平台 API、`cfg(windows)` / `cfg(target_os)` 一律不进本 crate。
- `image` 单独声明、不开 rayon；取时间走 `clock::now()`，别直接 `Instant::now()`。
- 测试专用 API 用 `#[cfg(any(test, feature = "test-util"))] pub`；wind-ui 的 dev-dependencies 开 `test-util`。
- 本 crate 的测试文字后端是 `MockText`（与 wind-ui mock 同公式：字符数 × 字号 × 0.6，高 1.2 × 字号）。
