# 主题 SVG / 图层 / 渐变：引擎能力盘点与编辑器对齐方案（GH#182）

跨仓：主仓 `WindInput`（`wind-theme` 求值、`wind-ui` 绘制）+ 主题编辑器 `WindInputThemeEditor`。

来源：[GH#182](https://github.com/huanfeng/WindInput/issues/182)「主题编辑器对 SVG 资源的支持不足」，
以及维护者补充的「引擎支持渐变、编辑器没有对应 UI」。

盘点基准：主仓 `05dd8cef`、编辑器 `a785bf2`。下文主仓路径相对 `wind_input/crates/`，编辑器路径相对仓根。

## 0. 结论先行

issue 的前提「运行时已具备 SVG 能力，只是编辑器没跟上」**只对了一半**：

| issue 诉求 | 引擎现状 | 编辑器现状 | 判定 |
|---|---|---|---|
| 1 导入 .svg | ✔ | ✔ `accept="image/*,.svg"` | 已满足 |
| 2 SVG 预览 | ✔ 全区域 | 候选窗 ✔；status/tooltip/toast/menu **完全不画图** | **编辑器缺** |
| 3 anchor/offset/size/opacity/z | ✔ | ✔ 但图层 UI 藏在实验开关后 | **编辑器缺**（入口） |
| 4 单侧尺寸保宽高比 | ✘ 两维独立，缺的一维取原图尺寸 | ✘ 同口径 | **两边都缺**，引擎先做 |
| 5 tint | 背景图 ✔、翻页箭头 ✔；**图层 ✘**（字段被丢） | 只有翻页箭头预览染色；无 tint 控件 | **两边都缺** |
| 6 data:image/svg+xml;base64 | ✔ | ✔ 内部存储即此形态 | 已满足 |
| 7 导出保留 .svg | —— | ✔ 原字节写 `assets/img-N.svg` | 已满足；**但导入回读有 bug，见 §3.1** |
| 8 共用解析逻辑 | resvg 0.44 | 浏览器原生 | 不做（见 §5） |
| 9 不支持特性提示 | —— | 无 | 可做轻量版 |

另有两条 issue 未提、但盘点中发现的实际问题：

- **`[resources]` 表引用 `assets/*.svg` 的主题，导出→导入后图片丢失**（§3.1）。issue 里给的示例恰好就是这种写法，
  很可能就是报告者「必须绕过编辑器手改 TOML」的直接原因。
- **SVG 不走 `mode`**：九宫格/平铺/居中对 SVG 一律退化为非等比拉伸（§1.2）。

渐变：引擎支持 linear/radial + 角度 + 多 stop，但**只接在候选窗系节点**；编辑器能读、能预览、能写回，**零编辑 UI**（§2）。

---

## 1. 引擎侧：SVG 与图片

### 1.1 资源声明与解析

- `[resources]` 是「名字 → 路径」映射，值可以是 `{ light, dark }`（`wind-theme/src/schema.rs:503`）。
- 视图里的 `ref` 解析顺序（`wind-ui/src/theme_assets.rs:9-26`）：resources 键 → `data:`/绝对路径 → 在 `asset_dirs`（自身 + base 链目录）按字面文件名查 → 原样返回。
- `data:` URI 只认 `;base64`，载荷中的空白/换行先剔除，TOML 多行字符串可用（`wind-ui/src/image_cache.rs:97-134`）。百分号编码、URL-safe base64 不支持。
- SVG 判定：文件看扩展名 `.svg`（不分大小写），`data:` 看 MIME `image/svg+xml`（`image_cache.rs:148-153`）。
- 位图格式：png / jpeg / webp / gif / bmp（工作区 `Cargo.toml:81-88`）。
- `.wtheme` 包内 `assets/` 不按扩展名过滤；限 200 条目、解压 32 MiB（`wind-transfer/src/theme.rs:49-61`）。

### 1.2 SVG 栅格化

- `resvg 0.44`（`wind-ui/Cargo.toml:37`），默认 feature 全开。按**目标像素尺寸**直接栅格化，任意 DPI 清晰；按 `(图, FillSpec, w, h, tint)` 缓存。
- **安全**：`<image href>` 的外部/本地文件引用被堵死（`image_cache.rs:44-52`），内嵌 `data:` 图片仍可用（usvg 默认行为，未有用例覆盖）。
- **`<text>` 不出字**：usvg 的 fontdb 默认是空库，本仓从未 `load_system_fonts`。主题作者须先把文字转成路径。
- 渐变、clipPath、mask、滤镜：resvg 内建能力，可用。
- **SVG 忽略 `mode`**：`build_fill` 的 SVG 分支直接 `rasterize_svg(path, w, h)`，不看 `FillSpec`（`image_cache.rs:454-467`）。`nine_slice`/`tile`/`center` 对 SVG 都等同 stretch。
- **非等比缩放**：`sx = w / 文档宽`、`sy = h / 文档高` 各自独立（`image_cache.rs:61-62`）。

### 1.3 定位与尺寸（背景图与图层共用 `ImagePlace`）

- `anchor`：子串匹配，含 left/right 定 x、含 top/bottom 定 y，否则居中（`wind-ui/src/view.rs:1679-1695`）。
  **空串 = 居中**；`view.rs:248` 注释写「空=top-left」，与实现不符。
- `offset {x,y}`：dp（×DPI）、`"Npx"`、`"N%"`（相对宿主）。
- `size {w,h}`：整数 dp，×DPI。**≤0 的那一维取原图/SVG 文档尺寸，且不乘 DPI**（`view.rs:264-283`，`theme_assets.rs:78-79`）。
  推论：200% 屏上不写 size 的图显得只有一半大；只写 w 时 h 不按比例推算。
- 背景图只要配了 anchor / 非零 offset / 非零 size 任一项，就按定位摆一块并裁到节点圆角内；否则铺满（`theme_assets.rs:87-94`）。
- 图层：强制 stretch（`mode`/`slice` 无效），裁到宿主矩形（**不带圆角**）（`view.rs:1637-1676`）。
- `z < 0` 画在内容下、`z >= 0` 画在内容及子节点之上（`view.rs:1207`、`:1315`）。
- 绘制顺序：底色 → 渐变 → 背景图 → 强调条 → 序号圆底 → z<0 图层 → 边框 → 文字 → 子节点 → z≥0 图层。

### 1.4 tint

- 语义：**把图当 alpha 蒙版**，输出 alpha = `src_a × tint_a`，RGB 换成 tint 色（`image_cache.rs:24-36`）。不是乘法混色。位图与 SVG 都适用。
- 颜色写法：hex、`${token}`（必须整串）、`{light,dark}`、`"transparent"`（alpha=0 → 视为不染色）（`wind-theme/src/resolve.rs:95-117`）。
- 生效范围：背景图 ✔、`footer_bar.prev_image/next_image` ✔（含 `disabled_tint`）。
- **图层 ✘**：`ViewLayer` 无 tint 字段（`view.rs:288-293`），`rv_layers` 不传，`paint_layer` 固定 `[0,0,0,0]`（`view.rs:1652`）。schema 有字段、resolve 有透传、渲染不消费——与 `theme-capability-extension.md` §0 是同型病。

### 1.4b flip（镜像）

- `flip = "x" | "y" | "xy"`：背景图（铺满 / 定位）、图层、`footer_bar.prev_image/next_image` 都认，status /
  tooltip / toast / menu.root 的背景图与图层同一结构、一并支持；缺省不翻，非法值告警后按不翻。
- **只翻像素**：按 `mode` / `slice` / `size` 合成到目标尺寸、染完色后整块翻（`ImageCache::fill`，
  `FillSpec.flip` 进缓存键）。`anchor` / `offset` / `size` / 九宫 `slice` 都按源图写、不翻。
- 推论：九宫格翻转后右缘显示的是源图左切片；`tile` 的起铺相位落在翻转后的那一侧（`x` 从右缘起铺）。
- 皮肤装饰同名字段见 `theme-skin-animation.md` §3.4；窗体 `frame` 不开放。

### 1.5 各区域支持矩阵

| 区域 | 背景图 | 图层 | 渐变 |
|---|---|---|---|
| window / preedit_bar / candidate_list / footer_bar | ✔ | ✔ | ✔ |
| item | 基态 ✔、选中态 ✔、**悬停态 ✘** | **✘ 不消费** | 同背景图 |
| text / comment / comment_above | ✔ | — | ✔ |
| status / tooltip / toast / menu.root | ✔ | ✔ | **✘ 不消费** |
| index / mode_label | 仅纯色（index 可 `shape="circle"`） | — | — |
| toolbar | ✘ | ✘ | ✘（`rvnode.rs:203-205` 注释「待补」） |

## 2. 引擎侧：渐变

- schema：`background.gradient = { type, angle, stops = [{ color, pos }] }`（`schema.rs:207-235`）。
  `type`：`"linear"`（默认）/ `"radial"`。只挂在 `background` 下；**边框、文字、阴影不支持渐变**。
- 求值：stop 颜色走 `resolve_color`；`pos` 钳到 [0,1] 升序；无有效 stop 返回 None（`resolve.rs` `resolve_gradient`）。
  缺 `pos` 的 stop 按序号均分（`i/(n-1)`，单个为 0），与编辑器同口径（全缺时同 CSS；与显式 pos 混写时不做邻值插值）；此前缺省为 0，全部挤在起点退化成纯色（审查发现，已改）。
- 绘制（`view.rs:1413-1487`）：1 个 stop 退化为纯色；linear 的 `angle` 单位是度，0 = 左→右，90 = 上→下（顺时针），端点按投影覆盖整个盒；
  radial 圆心固定在盒中心、半径 = 对角线一半，不可配；外延 Pad；裁到圆角内。
- 状态补丁（selected/hover/disabled）写了非空 stops 即算有覆盖（`resolve.rs:294-301`）。
- 内置主题与测试主题**全未使用**渐变。写法示例：

```toml
[item.selected]
background = { color = "${selection}", gradient = { type = "linear", angle = 90, stops = [
  { color = "${accent}", pos = 0.0 },
  { color = { light = "#FFFFFF", dark = "#000000" }, pos = 1.0 },
] } }
```

## 3. 编辑器侧现状（实施前快照，下列问题已由编辑器 `1b5db07` 修复）

技术栈：Vue 3 + Pinia + Vite；预览为 Canvas 2D，逐行对照 `wind-ui` 手写（`src/lib/preview/AGENTS.md`），不复用 Rust。

### 3.1 资源与导入导出

- 上传统一 `FileReader.readAsDataURL` 存成 base64 data URI，不转码（`ResourcesEditorV3.vue:204`、`ResourceRefField.vue:64`）。
- 导出：`extractDataUris` 在 TOML **文本层**把 data URI 拆成 `assets/img-N.<ext>`，原字节、按内容去重；扩展名由 MIME 反推，认不出的记 `png`（`src/shared/themePackage.ts:75-98`）。
- **Bug A（丢图）**：导入 `packageToTheme` 用 `mapImageRefs` 只改**键名为 `ref`** 的字段（`src/lib/theme3/package.ts:38-50`、`:101-107`）。
  `[resources]` 表的值不是 `ref` 键 → 停留在 `assets/...` 相对路径 → 预览加载失败。
  导出时 `extractDataUris` 是文本层全量替换，会把 resources 里的 data URI 一并拆出，所以**编辑器自己导出的包再导入也会触发**。
  现有用例只覆盖 `ref` 内联的情况。
- 包里未被 `ref` 引用的文件被静默丢弃；解包时未知扩展名按 `image/png`。
- **Bug B**：资源名含 `.`（如 `spark.svg`）时，`t3.setField` 按 `.` 拆路径存成嵌套对象（`stores/theme3.ts:405`、`ResourcesEditorV3.vue:129/224`）。
- **Bug C**：`findResourceRefs` 不扫 `prev_image`/`next_image`，重命名/删除资源时箭头引用不同步（`ResourcesEditorV3.vue:70-88`）。
- **Bug D**：`ImageFillEditor` 的 `resourceName` 按 `/` 切 data URI 取末段，内嵌图显示乱码（`ImageFillEditor.vue:69-73`）。
- 缩略图只对 `data:` 开头的值显示（`ResourcesEditorV3.vue:239-243`）。
- 待实测：只有 viewBox、没写 width/height 的 SVG 在浏览器里 `naturalWidth` 可能为 0，`getImage` 会判为不可用（`src/lib/preview/imageCache.ts`）。内置 chevron 就是这种写法。

### 3.2 图层与 tint

- 有图层 UI 的节点：window / preedit_bar / item / status / tooltip / toast / menu.root（`ViewsEditorV3.vue:306-309`），
  但**默认隐藏**，需开实验开关 `featureFlags.enableOverlayLayer`（`stores/ui.ts:236`）。
- `item` 开放了图层 UI，而引擎不消费（§1.5）——又一个「配了不生效」。
- 字段：模型完整（含 tint/disabled_tint）；UI 有 ref/mode/slice/opacity/z/anchor/offset/size，**无 tint 控件**；图层也能选 mode，但引擎和预览都按 stretch 画。
- 预览 z 规则：**z=0 不画**（`candidateBox.ts:559`），引擎是 z≥0 画在上层——不一致。
- tint 预览：`tintedImage`（`candidateBox.ts:461-487`，`source-in` 合成）只被翻页箭头用；通用 `toRVImage` 把 tint 写死 `null`（`src/lib/theme3/resolve.ts:134`），背景图写了 tint 预览不变色，而引擎会变。
- status / tooltip / toast / menu 的预览**完全不画背景图、图层、渐变**（`resolve.ts:454-506` 不读，`otherWindows.ts` 无绘制代码）。

### 3.3 渐变

- 模型 `GradientV3`、读入 `gradientF`、写出 `gradientVal`、预览 `paint`（`candidateBox.ts:691-724`）齐备，与引擎同口径。
- 状态补丁 `resolveState`（`resolve.ts:175-225`）不处理渐变——选中态渐变预览不出来，引擎会画。
- **零编辑 UI**。颜色控件只认 hex/token/`{light,dark}`。

### 3.4 往返保真

- 顶层与节点级未知键进 `extra` 保留；**内联表（background/image/layers 元素/gradient/border/shadow/offset/size）内的未知键全丢**。
- `.wtheme` 导入调 `reportImported` 未传原文，丢字段检测（`collectDroppedFields`）对这条路径失效（`EditorView.vue:149`）。

## 4. 方案

原则沿用 `theme-capability-extension.md`：**编辑器只开放引擎真正消费的能力，预览与引擎同口径**。
引擎缺的能力，要么引擎补上再开放，要么编辑器明确不开放。

### P0 编辑器修 bug（无需决策）

1. Bug A：导入时把 `[resources]` 的值也纳入 `assets/` → data URI 回填；补「resources 引用 assets/*.svg」往返用例。
2. Bug B：资源名校验（禁 `.`，或改按数组路径写入）。
3. Bug C：引用扫描补 `prev_image`/`next_image`。
4. Bug D：内嵌资源显示「内嵌 SVG/PNG · 大小」而非截断 base64。
5. 实测 viewBox-only SVG；若 `naturalWidth=0`，加载时从 SVG 文本读 viewBox 补尺寸。
6. `.wtheme` 导入路径接上 `collectDroppedFields`。
7. 预览 z=0 与引擎对齐（z≥0 画在上层）。

### P1 编辑器补齐已有引擎能力

1. 图层 UI 转正（移除实验开关），从 `item` 移除图层入口（引擎不消费）；图层不再显示 mode/slice 控件。
2. 背景图 tint 控件（颜色字段复用 `NodeColorField`），预览 `toRVImage` 解析 tint；翻页箭头补上 tint / disabled_tint 控件。
3. status / tooltip / toast / menu 预览补画背景图与图层（复用 `paintLayers`/背景图绘制）。
4. 渐变编辑器：类型（linear/radial）、角度、stop 列表（颜色 + 位置，增删拖拽）、实时预览；只在引擎消费渐变的节点开放
   （window/preedit_bar/candidate_list/footer_bar/item 基态与选中态/text/comment/comment_above）；`resolveState` 补渐变。
5. 资源面板显示类型与原始尺寸（SVG 读 viewBox）；上传 SVG 时检测 `<text>`、`<image href=非 data:>`，提示「不会显示」。

### P2 引擎补能力（需维护者定，见 §6）

1. 图层 tint：`ViewLayer` 加 tint，`rv_layers`/`paint_layer` 透传（改动小，与背景图同一套 `compose`）。
2. 单侧尺寸按原图宽高比推算另一维；缺省尺寸乘 DPI。**两项都改变现有主题的显示**，需评估。
3. SVG 支持 `center`（按文档尺寸×DPI 居中）、`tile`；`nine_slice` 对矢量意义不大，可明确不支持并在编辑器提示。
4. 渐变接到 status / tooltip / toast / menu（box 绘制已有，主要是 resolve/接线）。
5. 修 `view.rs:248` 的 anchor 注释。

每项引擎改动落地后，编辑器同步开放对应控件与预览。

## 5. 不做

- **编辑器复用 Rust 栅格化（WASM resvg）**：包体与工程代价大；浏览器 SVG 引擎对 issue 列的常用子集（path/rect/circle/fill/stroke/opacity/viewBox）与 resvg 结果一致。
  分歧点（`<text>`、外部 `<image>`）改用 §4 P1-5 的上传期提示兜住。
- **保留上传文件名**：内部统一 data URI 是既有设计（`package.ts:7-11`），导出 `img-N` 不影响引擎。如需可后续在 resources 键名上体现。
- **渐变边框 / 文字渐变**：引擎不支持，编辑器格式规格 §3.5 也已明确暂不做。

## 6. 已定（2026-10-12 维护者）

- P2-1 图层 tint：**做**。语义同背景图（alpha 蒙版），图层不支持 `disabled_tint`。
- P2-2 尺寸：**两项都改**。只给一维时另一维按原图宽高比推算；两维都不给时取原图尺寸 × DPI。
  位图与 SVG 同口径；九宫格 `slice` 仍按纹理像素，不受影响。
- P2-3 SVG `center` / `tile`：**做**。以 SVG 文档尺寸 × DPI 为单元居中 / 平铺；`nine_slice` 对 SVG 仍按 stretch，编辑器提示。
- P2-4 弹窗类渐变：**做**。status / tooltip / toast / menu.root 消费 `background.gradient`，绘制口径同候选窗节点。

实现附带的护栏（审查补）：定位图 / 图层目标尺寸每维钳到 4096 px（`view.rs` `MAX_PLACED_EDGE`），
SVG center/tile 只渲染目标盒可见部分，不先整单元栅格化——市场主题是不可信输入，超大 viewBox 或极端宽高比
不得造成无界分配。SVG 单元按统一比例缩放；DPI 只对 SVG center/tile 进缓存键。

## 实施状态：已实现（待真机核对）

| 项 | 主仓 | 编辑器 |
|---|---|---|
| P0 编辑器修 bug | — | `1b5db07`、`05c5d6d`（终审修排序） |
| P1 补齐已有能力 | — | `1b5db07` |
| P2 引擎补能力 | `e913939c`、`080db393`（stop 缺 pos 均分） | `1b5db07`（预览跟进） |

自动化验证：主仓 wind-theme 100 + wind-ui 554 全绿，Linux 与 xwin 交叉 clippy 零告警；
编辑器 1124 项全绿，`pnpm build`（含 check-engine-schema、vue-tsc）通过；浏览器实测图层 SVG + tint、
toast 背景图、渐变角度、导出再导入不丢图、SVG `<text>` 提示。

**真机待核对**：高 DPI 下 SVG center/tile 的清晰度与接缝；弹窗渐变观感；只写一维 / 未写 size 的第三方主题在新尺寸规则下的变化。

**后续**：渐变色标改为渐变条拖拽交互、图片资源可直接编辑 SVG 源码（编辑器 `29f755b`）。

**遗留**：预览侧平铺块数（20000）与离屏染色边长（2048/8192）是预览自定保护值，引擎无对应常量，仅病态主题会触及。
