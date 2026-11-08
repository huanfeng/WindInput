# 主题编辑器预览改用 wind-scene（wasm）

承接 `theme-skin-animation.md` §8–§9、`wind-scene-extraction.md` §6。编辑器仓 `WindInputThemeEditor`
现在的候选窗预览是 Canvas2D 手写重实现（`src/lib/preview/candidateBox.ts`），靠 parity 测试追引擎；
皮肤与动画让它再追一遍不现实。目标：编辑器的候选窗预览直接跑 core 的 `wind-scene`，像素与实机同源。

## 1. 取舍

| 问题 | 定案 | 理由 |
|---|---|---|
| wasm 里的文字 | **回调浏览器 Canvas2D**：`TextBackend` 的测量 / 绘制由 JS 用 `OffscreenCanvas` 完成，像素回读后合成进缓冲 | 不用分发 CJK 字体（Noto Sans SC 十几 MB），用的是用户机器上的系统字体——与该机器上输入法实际用的字一致；不用把 wind-ui 的 Linux 文字后端拆成独立 crate；wasm 体积小 |
| 主题来源 | 传 `theme.toml` 文本 + 资源表（相对路径 → 字节）；base 链里的内置主题编进 wasm | 与 core 同一套解析 / 合并（`wind_theme`），编辑器的 TS 合并逻辑不再参与预览 |
| 资源 | wind-scene 加内存资源源：相对引用先查调用方注册的资源表，再走原有 data: / 文件路径 | 主题包（图片在 `assets/`）与编辑器的 data: URI 两种形态都能预览 |
| 动画 | 调用方传 `t_ms`，wasm 按 `AnimClock` 出那一帧；事件经 `signal(name)` 喂状态机 | 编辑器自己 `requestAnimationFrame` 驱动，wasm 无计时器 |
| 产物分发 | WindInput 仓出构建脚本，编辑器仓**检入**构建好的 `.wasm` + JS 胶水（带来源提交号），`pnpm build:wasm` 从邻仓重建 | 编辑器部署链（EdgeOne / worker）不必装 Rust；版本可追溯 |
| 回退 | wasm 加载失败（老浏览器、无 OffscreenCanvas）时退回 Canvas2D 旧实现；旧实现在 wasm 跑稳一个版本后删 | 不因新路径让编辑器整页不可用 |

已知差异（接受）：浏览器的字形光栅与 DirectWrite / CoreText 不逐像素一致（抗锯齿、hinting）；行高取
`fontBoundingBoxAscent + fontBoundingBoxDescent`，与 DirectWrite 行高在个别字体上差 1 px。

## 2. 分期

### W1：wasm 入口 crate（WindInput）

- 新 crate `wind_input/crates/wind-scene-wasm`（`cdylib`，`wasm-bindgen`），只依赖 `wind-scene` / `wind-theme` /
  `wind-ui-types`；本机 `cargo check --target wasm32-unknown-unknown` 进 `dev.sh` 现有的 wasm 检查。
- `CanvasText`：实现 `TextBackend`，经 `#[wasm_bindgen] extern` 调 JS 提供的 `measure(text, font) -> [w, h]` 与
  `draw(text, font, rgba) -> {w, h, pixels}`；测量按 `measure_key` 缓存。JS 侧实现随产物一起出（`text.js`）。
- 内存资源源：`wind-scene` 加 `assets::MemoryAssets`（相对路径 → `Arc<[u8]>`），`theme_assets` 解析相对引用时
  先查它；不开 wasm 也能用（单测在本机跑）。
- 对外 API（JSON 进出，便于编辑器侧类型化）：
  - `Previewer::new()`；`set_theme(toml, assets)` → 解析错误原样返回（编辑器显示在面板）；
  - `render(opts)` → `{ width, height, rgba }`。`opts`：明暗、横竖、嵌入、上翻、`scale`、候选条数与内容、编码、
    选中 / 悬停、页码、`t_ms`；缺省给与 core 预览（`candidate_preview.rs`）同一套示例内容；
  - `signal(name)`：typing / page / commit / click 等，驱动装饰状态机。
- 测试：本机原生跑 API 层（文字用 mock）；`wasm-bindgen-test` 不引入，浏览器端由 W2 的编辑器测试覆盖。
- 构建脚本 `scripts/build-scene-wasm.sh`：`cargo build --release --target wasm32-unknown-unknown` →
  `wasm-bindgen --target web` →（有则）`wasm-opt -Oz`；输出体积，目标 < 3 MB（gzip 前）。

### W2：编辑器接入（WindInputThemeEditor）

- 产物放 `src/lib/preview/wasm/`（检入），`VERSION` 记 WindInput 提交号；`pnpm build:wasm` 调邻仓脚本重建。
- 预览组件：异步加载 wasm，成功则用它出 `ImageData` 画到预览画布，否则走旧实现；面板加「预览引擎」显示
  当前用的是哪条（便于报问题）。
- 主题文本：编辑器已有导出 TOML 的序列化，直接喂 `set_theme`；图片用编辑器内存里的字节，不再转 data: URI。
- 动画：预览画布 `requestAnimationFrame` 驱动 `t_ms`；「模拟事件」按钮调 `signal`。

### W3：皮肤可视化编辑（编辑器）

`theme-skin-animation.md` §9 第 2–4 项：在图上拖拉伸带 / 内容框 / 槽位 / 装饰锚点；动画帧区间与事件编辑。
W2 稳定后单独出设计。

### W4：删除旧实现

wasm 预览跑稳一个版本后删 `candidateBox.ts` 及其 parity 测试；`otherWindows.ts`（工具栏等其他窗口）不在本期。
