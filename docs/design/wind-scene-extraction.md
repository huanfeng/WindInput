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

## 7. P3c：主题预览图（`theme.renderPreview`）

过渡方案（`theme-skin-animation.md` §8）：设置端暂不进程内渲染，由服务端用实时候选窗的渲染器出 PNG，
像素与实机一致，不播动画。

### 7.1 接法

```
设置端 ──JSON-RPC──▶ wind-rpc ──data_rpc──▶ wind-webdata::theme_render
                                              │ 查缓存（命中直接回）
                                              │ 未命中：wind_theme 解析主题 → Coordinator::request_candidate_preview
                                              ▼
                       UiCommand::RenderCandidatePreview(请求 + SyncSender 回执) ──▶ wind-ui UI 线程
                       CandidateWindow::render_preview：effective_theme 叠当前外观覆盖 → render_candidate → PNG
                                              ◀── 回执（recv_timeout，超时 / 断开即报错）
```

- 协调器不碰渲染（headless 依赖门不变）：它只拼示例内容（编码 `nihao`，候选取「每页候选数」个、第三个带
  注释，序号按「用户 > 被预览主题 > 数字」）并投递；不支持预览的渲染端丢掉请求，回执断开，调用方立即得错。
- 外观覆盖（字体、字号、阴影开关、尺寸下限、翻页栏等）取 UI 线程当前状态，与实时窗口同一个合并函数；
  命令通道 FIFO，`config.setItems` 下发的设置命令先于随后的预览请求生效。预览用独立的 `CandidateScene`，
  不扰动实时窗口的跨帧状态。软件阴影按平台：ext_presenter（macOS / Linux 外部宿主）不画。
- 时限：3 秒 + 每张未命中 1 秒，封顶 20 秒（同一批共用）。

### 7.2 协议

`theme.renderPreview`（单条，失败即 RPC 错误）与 `theme.renderPreviews { items: [...] }`（批量，至多 64 条，
逐条回执，未命中的一次全部投给 UI 线程）。每条参数：

| 键 | 说明 |
|---|---|
| `name` / `package` / `text` | 至多给一个：已装主题 slug / 主题包路径（`.wtheme`）/ `theme.toml` 文本；都不给 = 当前主题。包与文本先解到临时目录（位图从包里取），base 链在已装主题里找 |
| `style` | `light` / `dark` / `system`；缺省按当前配置 |
| `layout` | `horizontal` / `vertical`；缺省按 `ui.candidate.layout`（含方案级覆盖） |
| `scale` | 0.5–4，缺省 1（设置端传自己窗口所在屏的缩放） |
| `inline` | 真则回执另带 `png`（base64）；取不到缓存目录时总是内联 |

回执：`{ ok: true, path, width, height, hash, cached }`（`path` 是缓存里的 PNG，设置端同机直接读）
或 `{ ok: false, error }`。

### 7.3 缓存

- 位置：`Config::cache_dir()/theme_previews/`（Windows 为 `%LOCALAPPDATA%\WindInput[Dev]\cache\theme_previews`，
  便携版在 `<exe>/localdata/cache/theme_previews`）。文件名 `<前缀>.<32 位十六进制键>.png`，前缀是已装主题的
  slug，包 / 文本为 `_pkg` / `_text`；临时解包目录在其下 `staging/`。
- 键：128 位 SipHash，覆盖渲染器版本（`wind_ui_types::CANDIDATE_PREVIEW_RENDER_VERSION`，示例内容或候选窗
  管线会改像素时递增）、外观指纹（`ui.candidate` 含方案级覆盖、`ui.font`、方案级候选字体，有意放宽）、
  明暗、横竖、缩放，以及 base 链每个主题目录的全部文件（相对路径 + 字节，与目录位置无关）。主题里用绝对
  路径引用目录外的图不在键内（少见，改了要等清理或升版本）。
- 命中刷新 mtime。清理：每个服务进程第一次用到预览时一次，之后每批有新图落盘时一次；闲置超过
  **30 天**的删，余下超过 **50 MiB** 按 mtime 从旧到新淘汰；崩溃残留的临时文件 / 解包目录超过 1 小时删。
  `theme.delete` 顺带删该主题的图。常量在 `wind-webdata/src/theme_render.rs`。

### 7.4 设置端（发版后再做）

- **分组**：新建「主题」分组，排在「外观」之后（按 manifest `groups` 的 order 插一个值）；`ui.theme.name` /
  `ui.theme.style` / `ui.theme.animation` 从「外观 → 主题」卡片移过去。新 label 补拼音首字母表。
- **画廊**：`theme.list` 列出全部已装主题，一次 `theme.renderPreviews`（`scale` = 设置窗口所在屏缩放，
  其余缺省）取缩略图；网格自适应列数（每格宽约 260 逻辑像素，图按宽缩放、保持比例，下方主题名，当前主题
  描边 + 勾），点击写 `ui.theme.name`。删除 / 导入 / 导出入口放在页头或每格的菜单里。图用 windui
  `image_bytes` 从 `path` 读；读不到或回执失败的格显示占位文字（服务端不可用同此，不弹窗）。
- **刷新**：收到 `config.changed` 且变更键落在 `ui.theme.*` / `ui.candidate.*` / `ui.font.*`，或主题列表变化
  （导入 / 删除）时，防抖约 300 ms 后整批重取；未变的主题服务端直接命中，代价只是一次往返。
- **导入预览**：`theme_import_dialog` 拿到包路径（或文本）后，与 `theme.previewPackage` 并行发
  `theme.renderPreview { package | text }`，在确认按钮上方显示图；失败只隐藏图，不挡导入。
- **mock**：`src/rpc.rs` 的 mock 给两个方法各回一张内联小 PNG。
