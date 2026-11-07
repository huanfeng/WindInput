# wind-scene 抽取方案

`theme-skin-animation.md` §8 的落地方案：把候选窗「纯数据 → View 树 → 像素」从 `wind-ui` 抽到平台无关的
`wind-scene`，给实时窗口、设置端预览、主题编辑器（wasm）共用。

## 1. 现状

- 渲染管线在三处各写一份：非 Windows `show`、ext_presenter `render_frame`、Windows `render_frame`（Windows `show` 只是贴图）。
  每帧：取 DPI → `refresh_skin_geom`（读 `Instant::now`、跑状态机）→ `build_tree(false)` → `skin_layout` / `root.layout`
  → 宽度钳制 → `skin_fit_frame` → 定位（决定上翻）→ 上翻时 `build_tree(true)` 重排 → 命中矩形 → 绘制（软件阴影、皮肤底层、根、槽位、皮肤上层、Windows 拆 pass 层）。
- 平台分支全在窗口侧；`build_tree` 里唯一的平台调用是 `content_budget_px` 间接取屏幕工作区宽度。
- 文字后端是按平台换的同名具体类型（DirectWrite / CoreText / Linux 纯 Rust / mock），没有 trait。
- 全局状态：`view.rs` 的 `IMAGE_CACHE` / `SHADOW_CACHE`（thread_local），Linux 字体 `STORE`。
- tiny-skia：wind-ui 0.11、windui 0.12；resvg 0.44 对 0.48。故 wind-scene 公开 API 只收发 BGRA 缓冲，不暴露 tiny-skia 类型。

## 2. 目标形态

- `CandidateSceneInput`（每帧内容 + 排布 + 生效主题与尺度 + 屏幕工作区尺寸 + 低频选项 `CandidateSceneOptions`），
  `AnimClock { t, showing, allowed }` 从外部注入时间。
- 有状态的 `CandidateScene`：持有皮肤几何、装饰状态机、槽位等跨帧状态；API：`layout` / `relayout_above` / `place_anchor` /
  `shadow` / `hits` / `paint` / `decor_event` / `next_anim_at`，外加一次出图的 `render_candidate`（设置端、编辑器用）。
- `trait TextBackend`（`base_size` / `measure` / `draw` / `draw_runs`），各平台后端在 wind-ui 实现。
- wind-scene 不依赖 wind-config（uuid / dirs 挡 wasm）；`image` 单独声明、不开 rayon；时间经 `clock::now()` 垫片。

## 3. 分期

| 子阶段 | 步骤 | 内容 |
|---|---|---|
| P3a | S0 | 先补金样（G1–G6，见 §4），只改 `render_golden.rs` 与 `testdata/` |
| | S1 | 建 `crates/wind-scene` 骨架，登记 workspace 与 AGENTS.md crate 索引；`test-util` feature 暴露测试专用 API |
| | S2 | 文字公共类型（`TextMetrics` / `TextStyle` / `ColorRun` / `utf16_runs` / `measure_key`）搬入 `wind_scene::text`，加 `TextBackend`，四个后端转发实现；原路径 `pub use` |
| | S3 | `text/script.rs`、`span_runs.rs` 搬走（依赖 wind-config 的那条测试留 wind-ui） |
| | S4 | `view` / `image_cache` / `skin` / `skin_anim` / `theme_assets` 一次搬（互相依赖）；`lib.rs` `pub use` 保留原路径；`recv_evicting_idle*` 留 wind-ui；`MockText` 照抄 mock 公式；加 `clock::now()` |
| P3b | S5a | wind-ui 内先抽 `candidate_window/scene.rs`（`build_tree` 等改成 `impl CandidateScene`，`self.x` → `i.x`） |
| | S5b | `skin_frame.rs` 拆两半：几何/布局/绘制/避让/对齐点进场景，事件接线留窗口 |
| | S5c | 三处管线合并公共段 |
| | S5d | 跨 crate 搬到 `wind_scene::candidate` |
| | S6 | 生效主题纯函数下沉（`effective_theme`，字体解析以闭包注入），设置端拿到同一份生效主题 |
| P3c | S7 | `render_candidate` + 场景级金样；设置端预览；wasm check 门；Linux 文字后端 `FontSource` 抽象；`wind-text` 拆分取舍 |

每步可编译、测试与金样逐字节不变、单独提交。S5c 之后必须上 Windows 靶机看本地窗口与 host-render 两条路径（Linux 测试覆盖不到）。

## 4. 金样缺口（S0）

| 编号 | 缺口 | 补法 |
|---|---|---|
| G1 | 金样绕过 `skin_layout`、宽度钳制、`skin_fit_frame`、软件阴影、命中矩形 | 走非 Windows `show()` 录 mock 窗口缓冲摘要、`hit_rects`、margins、皮肤鼠标矩形 |
| G2 | 出厂主题都没有 `[skin]` | 入库程序生成的小夹具主题：横竖、嵌入变体、分槽位、三档 z、`hit=pass`、avoid 遮罩、对齐点、1.0/1.5/2.0 |
| G3 | 无动画帧金样 | 时间可注入（`refresh_skin_geom_at` / `skin_event_at`），录首帧、帧中、事件后、片段中与落定后、关动画首帧 |
| G4 | 只录了 `build_tree(false)` | `above=true` × flip/swap × 横竖 |
| G5 | 只有 scale 1.0 | default 主题补 1.5 / 2.0 |
| G6 | grid 展开、`min_*`、只有模式徽标、`pager_in_preedit` / 隐藏翻页 | 各补一例 |

## 5. 风险

- 图片缓存是 thread_local，`manager.rs` 的闲置回收与截止时刻登记必须继续引用同一组函数，漏登记则回收永不推进。
- 动画：状态机跨帧存活、几何每帧重算、`skin_event` 读上一帧几何的变体，S5b 顺序弄错会「偶尔不播或播错帧」。
- DPI：`scale` 与 `set_base_size` 两个副作用场景化后可能不一致，用 `SizedText` 固定 `base_size`。
- ext_presenter 只在 `linux-host` / macOS 下编译，默认 `cargo test` 看不到。
- `candidate_window.rs` 上万行，P3b 与皮肤开发冲突面大，放在 P5 收尾后。

## 6. wasm 前瞻（P3c，本期只保证不堵路）

- `Instant::now` 在 wasm32 运行时 panic → `clock::now()` 垫片换 `web-time`。
- `std::fs` 返回 Unsupported → 编辑器用 `data:` URI；以后可加缺省开的 `fs` feature。
- Linux 文字后端用 mmap / `AsRawFd` / dlopen，编不过 → 抽 `FontSource`，系统字体实现放 `cfg(unix)`，新增内存字体实现。
- 设置端文字：拆独立 `wind-text` crate（DirectWrite 同源、像素一致），或给 windui 自带的 DirectWrite 写适配（有度量差异）。
