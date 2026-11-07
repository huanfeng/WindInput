# 主题异形皮肤与动画

跨仓：主仓 `WindInput`（`wind-theme` 求值、`wind-ui` 绘制、`wind-transfer` 包格式）、
主题编辑器 `WindInputThemeEditor`、设置端 `wind-setting`。下文主仓路径相对 `wind_input/crates/`。

状态：设计稿 + P1 原型（分支 `feat/theme-skin`）。未发布前 schema 可直接改，不做兼容。

P1 原型已落地（§12）：schema、多段切片 + round、density / srcset / scale_with、面积平均缩放、
装饰外扩与 block、对齐点平移；三条渲染路径（Windows、ext_presenter、非 Windows）都已接入。

## 0. 目标与结论

1. **异形皮肤**：候选窗背景由「皮肤层」决定整体外形，可由多张图组合；拉伸按多段切片进行，
   花边按整数次重复，不被切半；内容（编码 / 候选 / 翻页 / 提示）按槽位摆放；装饰（宠物挂件）
   可伸出窗体、不参与排版、不干扰输入。横竖排各自一套，互不深合并。
2. **位图 DPI**：皮肤声明源图密度，切片与槽位随 DPI 等比缩放；缩小用面积平均，不再最近邻。
3. **动画**：装饰层支持 APNG / 动画 WebP（首选）、GIF（兼容），可按输入事件切换片段。
4. **主题包优先**：图片多的主题以 `.wtheme`（zip，图片为文件）为主，base64 仅兼容。
5. **渲染逻辑共享**：候选窗场景渲染抽成平台无关 crate，实时窗口、设置端预览、编辑器（wasm）
   三处共用一套代码（§8）。

已定的取舍（维护者 2026-11-07）：

| 问题 | 结论 |
|---|---|
| 装饰鼠标策略 | P1 单窗口 `hit = "block"`；P2 拆第二个窗口实现 `pass`（点击穿透） |
| 编辑器预览 | 评估把 Rust 渲染器编成 wasm，以 §8 的共享 crate 为前提 |
| 缩放基准 | 两种都支持：`scale_with = "dpi"`（默认）/ `"font"` |

## 1. 参考：常见的位图皮肤格式

主流输入法的异形皮肤大多是「一张或几张大位图 + 一份 ini」：

- 每轴用「左（上）固定长度 + 右（下）固定长度」切成三段，只有中间一段可拉伸；装饰烘焙在图里，
  靠挑拉伸位置躲开装饰。
- 编码区、候选区各有一组上下左右内边距，纵向堆叠。
- 「编码在窗内」与「编码嵌入」、横排与竖排各一套，横竖是完全不同的图。
- 有字号基准，皮肤整体随字号缩放。素材通常远大于显示尺寸，显示时缩小。

我们要覆盖这些能力，并补上「多段拉伸」「装饰分层」「装饰伸出窗体」。
第三方皮肤只作本地参考，**素材与参数都不入库**；入库的夹具与示例主题用程序生成或原创素材。

## 2. 现状（盘点基准 `728d29f4`）

已有：九宫格 + `slice_repeat` 平铺（`wind-ui/src/image_cache.rs:599-669`）、`layers` 叠图（九方位锚点）、
`[节点.vertical]` 竖排逐节点覆盖、`window.position_offset`、`.wtheme` zip 包（`assets/**` 为文件）。

| 差距 | 位置 |
|---|---|
| 切片单位是纹理像素、不随 DPI | `wind-theme/src/resolve.rs:127` |
| 位图逐像素最近邻采样 | `image_cache.rs:570` |
| 每轴一段拉伸；平铺不取整 | `image_cache.rs:643` |
| layers 被裁到宿主盒内 | `wind-ui/src/view.rs:1906-1938` |
| 无内容槽位，只能靠 padding | — |
| 竖排覆盖是逐节点深合并，换不了整张图 | `wind-theme/src/theme.rs:136-185` |
| 命中区外一律拖动；alpha>0 都接鼠标 | `candidate_window.rs:5118-5230` |
| 屏幕钳制按内容尺寸 | `candidate_window.rs:2512` |
| Wayland 输入区是一个矩形；macOS 系统阴影随 alpha 形状 | `wind_linux/src/fcitx/WaylandPanel.cpp:1315`、`CandidatePanel.swift:251` |
| 无动画：GIF/APNG 只取首帧，缓存键无帧号，UI 线程纯事件驱动 | `image_cache.rs:173`、`manager.rs:1166` |
| 编辑器图片一律 base64；预览是 Canvas2D 手写重实现 | 编辑器 `candidateBox.ts`（2374 行） |

旧的 `nine_slice` 已正式发布，语义保持不变；皮肤是新增概念，不改旧字段。

## 3. 皮肤层 schema

### 3.1 总览

```toml
[skin]
scale_with = "dpi"        # dpi（默认）| font
density = 3               # 源像素 / dp，可被各图覆盖；缺省 1
font_base = 16            # scale_with = "font" 时：生效字号等于它时按 density 1:1

[skin.horizontal]         # 横排皮肤；缺省则横排不启用皮肤
# [skin.vertical]         # 竖排皮肤；缺省则竖排不启用皮肤（回退普通窗口，不硬套横排图）
# [skin.horizontal_embedded] / [skin.vertical_embedded]
#                         # 编码嵌入候选行时的变体；缺省回退对应的非嵌入变体

[skin.horizontal.frame]
image = "frame_h"         # resources 键或包内路径
stretch_x = [[300, 500]]  # 源像素区间，可多段
stretch_y = [[400, 440]]
fill = "stretch"          # 拉伸带填法：stretch | tile | round；可逐段覆盖
# stretch_x = [{ from = 120, to = 180, fill = "round" }, { from = 900, to = 960 }]

[skin.horizontal.content]   # 内容框：相对窗体四边的内缩（源像素）
left = 180
top = 320
right = 200
bottom = 120

[[skin.horizontal.decor]]
image = "tiger"
anchor = "frame.top_right"  # 窗体 / 内容框 / 槽位上的锚点
pivot = "bottom_right"      # 装饰自身对齐点；缺省同 anchor 的方位
offset = [-80, 20]          # 源像素
z = 1                       # <0 在窗体下；0..99 在窗体上、内容下；>=100 在内容上
hit = "drag"                # drag（缺省）| block | pass，见 §4

[skin.horizontal.placement]
caret = "frame.top_left"           # 窗口在光标下方时，与光标对齐的点（缺省即此）
caret_above = "frame.bottom_left"  # 在光标上方时（缺省即此）
# 也可写成锚点 + 偏移（源像素），指到图里任意一点，如「内容框左缘、框沿上边」：
# caret = { anchor = "frame.top_left", offset = [180, 260] }
avoid_caret = true                 # 装饰不得盖住光标所在行（缺省开）
clamp = "frame"                    # frame（缺省）只保证窗体在屏内 | all 连装饰一起
```

启用皮肤后，`views.window` 的背景、边框、圆角、阴影不再生效（阴影由图自带），其余节点
（编码栏、候选项、选中态等）照旧，作者通常把它们的底色设为透明。

### 3.2 窗体（frame）：多段切片

把源图沿每轴切成「固定段」与「拉伸带」交替的序列（Android 9-patch 的推广）：

- 固定段 1:1 绘制（按缩放倍率），不变形。
- 窗体比最小尺寸多出的长度，按各拉伸带的源长度比例分配。
- 拉伸带填法（`fill` 设各带缺省，带上可逐个覆盖）：
  - `round`（**拉伸带缺省**）：取最接近的整数次重复，再把单元微调缩放到正好铺满。带里的针脚、纸纹、
    花边按原节奏重复，与两侧固定段对得上，也不会出现半个图案。
  - `stretch`：双线性拉伸。只适合沿该轴完全均匀的区域；带里有任何花纹都会被拉长、和固定段对不上
    （实测框沿虚线针脚被拉长一截，一眼可见）。没写拉伸带、整轴缩放时缺省用它。
  - `tile`：原样平铺，末尾裁掉。
- 最小尺寸 = 固定段之和（按倍率），由内容撑大，不会缩到比它小。可选 `max_width` /
  `max_height`（dp），超出时内容按现有截断规则处理（**未实现**）。
- 没写 `stretch_x` 的轴整体拉伸；写成空数组 `[]` 表示该轴不可伸缩（固定尺寸皮肤）。

### 3.3 内容框与槽位

内容框（`content`）用相对窗体四边的内缩定义，随窗体伸缩。

**P1：`stack` 布局**（缺省）。编码栏、分隔线、候选列表、翻页栏仍按现有排版纵向堆叠，整块
放进内容框。窗体尺寸 = 内容尺寸 + 四边内缩，再取与最小尺寸的较大者。常见格式的两组内边距
等价于「内容框 + 编码栏 margin」，这一层已能覆盖它。

**P1b：`free` 布局**。写了 `slots.candidates` 即启用；各部分各占一个槽位、分别锚定：

```toml
[skin.horizontal.slots.preedit]      # 缺省：编码栏照旧叠在候选上方，一起进候选槽
anchor = "content.top_left"          # 锚点只认 frame.* / content.*，缺省 content.top_left
[skin.horizontal.slots.candidates]   # 必填；候选列表（竖排时含底部翻页）
anchor = "content.bottom_left"
offset = [0, 0]                      # 源像素
[skin.horizontal.slots.footer]       # 缺省：翻页栏按原有排版留在候选块里
anchor = "frame.bottom_right"
pivot = "bottom_right"               # 缺省同锚点位置
offset = [-200, -40]
```

槽位按内容自然尺寸从锚点沿 pivot 反方向生长。窗体尺寸逐轴取闭式最小值，需满足三条：
一是不小于图的最小尺寸，二是每个槽位落在它所锚的框内（锚内容框的槽位不越出内容框，锚窗体的不越出窗体），
三是满足 stack 时的内容尺寸下限。槽位互相重叠由作者负责，编辑器给出警告。

free 布局下不做上翻交换：槽位由皮肤决定，候选窗翻到光标上方时只换对齐点，不重排内容。
`hint`（上方注释、模式提示）槽位暂不做，仍随候选块排版。tooltip 和状态提示是独立窗口，不在本设计内，
沿用现有九宫格。

### 3.4 装饰（decor）

- 不拉伸，按 `anchor` + `pivot` + `offset` 定位；锚点可以是 `frame.*`、`content.*`、`slot.<名>.*`（**未实现**，目前只认前两种）
  的 9 个方位。
- 可伸出窗体：窗口画布按所有装饰的外包围盒外扩，和现有阴影外扩 margin 合并处理。
- `z` 分三档（窗体下 / 窗体与内容之间 / 内容之上）。盖在文字上的装饰要作者自觉，编辑器提示。
- 装饰也可以用 `[[skin.*.decor]]` 引用动画（§6）。

### 3.5 横竖与嵌入的独立性

- `skin.horizontal`、`skin.vertical` 是两套完整定义，不互相继承、不深合并。
- 竖排没配皮肤时不启用皮肤（回退普通窗口样式）。异形图没法同时兼容两种形态，宁可不套。
- 嵌入变体（`*_embedded`）缺省回退对应的非嵌入变体。
- 主题 `base` 继承仍在 Value 层深合并：子主题写了 `skin.horizontal` 就整表替换 base 的同名表
  （皮肤是整体设计，逐字段合并没有意义）。

### 3.6 定位

- 对齐光标的是 `placement.caret` 指定的点，不是图片左上；上翻时用 `caret_above`。缺省是窗体左上 /
  左下角：窗体本身不会压住光标所在行。改成 `content.*` 能让文字贴着光标，代价是窗体上半截盖住正在
  输入的那一行，作者按图自己取舍。
  装饰烘焙在窗体图内的版式，用「锚点 + 偏移」把对齐点指到框沿上：文字横向贴光标，
  纵向只有透明区与装饰越过光标行。
  实现：对齐点、避让保护带、钳制外扩一起作为 `PlaceAnchor` 传进 `place_window`，翻转判定与屏幕钳制都按
  对齐后的位置算；无皮肤时为默认值，行为与改前逐位一致。
- `avoid_caret`：在光标行（`caret_y - caret_h .. caret_y`，上下各留 2px）上取一条保护带，横向从对齐点
  向左 `before`、向右 `after`（dp，缺省 200 / 40：左边是刚输入的文字，留得宽）。窗体与全部装饰合成后，
  保护带里只要有 alpha ≥ 32 的像素，就把窗口沿远离光标的方向逐像素推开，直到保护带清空。
  透明区、远离光标的装饰越过光标行不算遮挡；烘焙在窗体图里的花瓣、框沿压到已输入的字会被推开。
  写法：`true`（缺省）/ `false` / `{ before = 200, after = 40 }`。
- `clamp`：屏幕钳制按窗体（默认）或连同装饰计算。
- 保护带只在定位时按当前尺寸合成一次（图片走缓存），横坐标取对齐前的对齐点；水平钳制把窗口推离
  对齐点时保护带不随之重算。
- `position_offset` 继续叠加，语义不变。

## 4. 鼠标

| 区域 | P1 | P2 |
|---|---|---|
| 槽位内的候选 / 翻页 | 现状 | 现状 |
| 窗体空白（含内容框空白） | 拖动（现状） | 同左 |
| 装饰 `hit = "drag"`（缺省） | 不透明处接住鼠标，按下拖动候选窗 | 同左 |
| 装饰 `hit = "block"` | 接住鼠标、不拖动、不悬停（贴纸） | 同左 |
| 装饰 `hit = "pass"` | 按 block 处理 | 点击穿透到底下的应用 |
| 全透明像素 | 系统穿透（layered 窗口天然行为） | 同左 |

为什么 `pass` 要拆窗口（Windows）：单个 layered 窗口只有 alpha=0 的像素能穿透；`WM_NCHITTEST`
返回 `HTTRANSPARENT` 只对同线程窗口有效，对别的进程的窗口无效。所以 P2 把 `pass` 装饰放进第二个
`WS_EX_LAYERED | WS_EX_TRANSPARENT` 窗口，和候选窗同步位置、显隐和 z 序。好处是动画只重绘这个小窗口。
代价是两窗同步：移动走同一次 `DeferWindowPos`，显隐同帧。

其他平台：macOS 第二个 panel `ignoresMouseEvents = true`，并关掉系统阴影或只给窗体算阴影；Wayland
输入区改为「窗体矩形 − pass 装饰」；X11 有合成器时 XShape INPUT 按 alpha 掩码扣掉 pass 装饰。

缺省取 `drag`：实测 `pass` 让看起来实心的人物「摸不到」，体感奇怪；拖人物等于拖窗口最符合直觉。
`pass` 留给雪花、光斑这类氛围装饰。点击互动（点一下换表情等）留给 §6 的事件，届时另加字段，不复用 `hit`。

判定「不拖动的装饰区」：`block`（及回落画进主窗口的 `pass`）装饰的外接矩形与窗体矩形求差，命中 → 不拖动、不悬停。

## 5. DPI 与缩放

- **倍率**：`k = 渲染缩放 / density`，其中渲染缩放 = DPI 缩放（`scale_with = "dpi"`），
  或 `DPI 缩放 × 生效字号 / font_base`（`"font"`）。切片、内容框、槽位、装饰偏移都是源像素，统一乘 `k`。
- **多分辨率**（可选）：
  ```toml
  [skin.horizontal.frame]
  image = "frame_h"
  srcset = [{ image = "frame_h_1x", density = 1 }, { image = "frame_h_2x", density = 2 }]
  ```
  取「density ≥ 所需」中最小的一份；都不够就取最大的一份放大。坐标始终按主 `image` 的 density 写，
  其他份按比例换算，作者只标一次。
- **重采样**：整张源图先按 `k` 缩成目标图（预乘 alpha 后做面积平均，放大用双线性），再在目标像素空间做
  §3.2 的切片映射：固定段直接拷贝、拉伸带双线性、tile/round 平铺。切片边界按同一套取整规则换算，避免接缝。
  缓存键 `(图, k 的千分值)`；源图缩完即释放。
- **内存**：1800×900 的 RGBA 解码约 6.5 MB，缩到 1/2 后约 1.6 MB。加载时只常驻目标尺寸，
  源图在缩放完成后丢弃，计入现有 32 MiB 源图预算（`image_cache.rs:272-289`）。

## 6. 动画

### 6.1 格式

| 格式 | 结论 | 依据 |
|---|---|---|
| APNG | 首选 | 无损、8 位 alpha；`image` 0.25 的 `PngDecoder::apng()` |
| 动画 WebP | 首选 | 无损 + alpha，通常比 APNG 小；`image` 0.25 webp 解码器实现 `AnimationDecoder` |
| GIF | 兼容 | 1 位 alpha、256 色 |
| 精灵图 | 可选 | PNG 网格 + `frame_size` / `fps`，实现最简，编辑器易做（**未实现**） |
| SVG 动画 | 不做 | resvg 不支持 SMIL / CSS 动画 |
| Lottie | 暂不做 | 依赖重 |

### 6.2 声明

```toml
[[skin.horizontal.decor]]
image = "tiger"            # 动画文件；静态图同字段
anchor = "frame.top_right"
fps = 24                   # 可选，覆盖文件自带帧时长
[skin.horizontal.decor.states]
idle   = { frames = "0-23", loop = true }
typing = { frames = "24-35", loop = true }
[skin.horizontal.decor.on]
commit  = { play = "36-47", then = "idle" }
page    = { play = "48-55", then = "@prev" }
mode_en = { play = "56-63", then = "idle" }
idle_5s = { state = "idle" }
```

- 片段可以写帧区间，也可以写另一个文件（`play = { image = "tiger_jump" }`）。
- 状态机：当前状态循环播放；事件触发一次性片段，播完转 `then`（`@prev` 回原状态）；正在播的一次性片段
  被新事件打断时直接切换。
- 事件：`show`、`hide`、`typing`（编码变化）、`backspace`、`page`、`select`（数字键 / 点击选词）、
  `commit`、`empty`（空码）、`mode_cn` / `mode_en`、`idle_<N>s`；`click`（点中本装饰：只对 `drag` / `block`
  装饰，按下后未超过拖动阈值即松开才算，移动了按拖动处理）。
  其中 `commit`、`mode_*` 目前不经过 UI 线程，需在 `wind-ui-types` 加一条纯数据的 `UiCommand`。

### 6.3 运行时

- 加载时解码成目标倍率的帧（同 §5），每个主题动画帧预算 24 MiB，超出则降帧率（隔帧丢弃）并 warn。
- 帧率上限 30；窗口隐藏即暂停。
- 设置项「关闭主题动画」（默认开），同时跟随系统减少动态效果（Windows `SPI_GETCLIENTAREAANIMATION`、
  macOS `accessibilityDisplayShouldReduceMotion`），关闭时停在 `idle` 首帧。
- 驱动：在 `manager.rs` 的 `next_deadline` 清单登记下一帧时刻。
- 重绘：静态部分（窗体 + 文字）缓存成一张底图，换帧只重新合成装饰矩形；P2 拆窗后只刷装饰窗口。
- macOS / Linux 走 SHM 推帧，帧率减半；后续再评估由前端本地播放。

## 7. 主题包

- 皮肤主题以 `.wtheme` 为主：包内图片是文件，PNG/WebP/GIF 用 zip 存储模式（不再压缩）。
- 皮肤图片放 `assets/`，引用写成 `assets/文件名`（直接写或经 `[resources]` 键）。导出时引用越出
  `assets/` 或文件不存在就报错并指出是哪个引用，不自动搬运改写：改写会让包里的 `theme.toml` 与作者
  目录里的不一致，还要处理重名。导入时包里缺皮肤引用的资源同样报错，在落盘前拦下。`base` 父主题
  照旧不进包，子主题皮肤引用的图也要放在自己的 `assets/` 里。
- 上限：总解压 32 → 64 MiB；条目 200 不变；单个图片 16 MiB（文本条目 4 MiB 不变）。
- 纯 TOML + base64 仍能加载，但编辑器对含位图皮肤的主题提示「导出为主题包」。

## 8. 渲染逻辑共享（实时窗口 / 设置端 / 编辑器）

现状是两套实现：主仓 `wind-ui` 与编辑器 Canvas2D 重写版，靠 parity 测试兜底。皮肤与动画会让
重写量翻倍；设置端也要预览。三方共用一套 Rust 实现：

```
wind-theme (求值)  →  wind-scene (新, 平台无关)  →  RGBA 缓冲
                         ├─ View 树 / 布局 / 绘制（现 view.rs）
                         ├─ 图片缓存、皮肤切片、动画帧（现 image_cache.rs / theme_assets.rs + 新增）
                         ├─ 候选窗场景构建（从 CandidateWindow::build_tree 抽出，输入为纯数据快照）
                         └─ trait TextRenderer（测量 + 绘制）
消费方：
  wind-ui        —— 实时窗口；文字后端 DirectWrite / CoreText / Linux 纯 Rust
  wind-setting   —— path 依赖 wind-scene，输出缓冲交给 windui 的图像控件；Windows 同用 DirectWrite
  编辑器         —— wind-scene 编 wasm；文字后端用纯 Rust（ttf-parser + rustybuzz，现 text/linux/），
                    字体字节由浏览器加载后传入
```

- 抽取的关键是 `CandidateWindow::build_tree` 依赖窗口状态；改为输入一份 `CandidateSceneInput`
  （候选、编码、选中 / 悬停、横竖、上翻、DPI 缩放、生效配置），产出 View 树。窗口类只负责定位、鼠标、上屏。
- 现有 `render_golden` 金样（mock 文字后端）是抽取的安全网：抽取前后逐字节一致。
- 版本对齐：wind-ui 用 tiny-skia 0.11，windui 用 0.12。设置端依赖 wind-scene 会编两份，先接受，
  后续把 workspace 升到 0.12。
- 过渡方案：抽取完成前，设置端可经 RPC `theme.renderPreview`（core 用真实渲染器出 PNG）先上线预览，
  像素与实机一致但不适合播动画。抽取完成后改为进程内渲染。
- wasm 评估项：体积（目标 < 3 MB gzip 前）、纯 Rust 文字与 DirectWrite 的度量差异（预览可接受轻微差异）、
  `image` / `resvg` / `tiny-skia` 在 wasm32 的编译与性能。

## 9. 编辑器

1. 资源模型：data URI → 虚拟文件系统（IndexedDB），导出直接打包；这是编辑器侧地基。
2. 可视化：在图上拖拉伸带、内容框 / 槽位、装饰锚点；预览切换内容（1 / 9 候选、长编码、嵌入）与 DPI（100/150/200%）。
3. 横竖、嵌入变体分页签编辑。
4. 动画：帧区间、状态、事件编辑，配「模拟事件」按钮。
5. 预览按 §8 切到 wasm 后，删除 `candidateBox.ts` 的重复实现。
6. 可选：第三方皮肤导入（只供本地自用，提示版权）。

## 10. 分期

| 期 | 内容 | 验收 |
|---|---|---|
| P1 | schema（§3.1–3.2、3.4–3.6 的 stack 布局）；多段切片 + round；density/倍率 + 面积平均；装饰外扩 + block；对齐点与 clamp | 生成夹具在 100/150/200% 下金样稳定；示例皮肤本机目视还原 |
| P1b | `free` 槽位布局 | 编码与候选分区锚定 |
| P2 | pass 拆窗、`avoid_caret`；macOS / Wayland / X11 适配 | 三平台穿透正确 |
| P3 | §8 抽取 `wind-scene`；设置端预览；wasm 评估 | golden 不变；设置端出图 |
| P4 | 动画运行时 | 关闭开关、预算、CPU 占用 |
| P5 | 事件状态机 + `UiCommand` 信号 | 上屏 / 翻页触发片段 |
| P6 | 编辑器（§9）、包体上限、市场、文档站 | — |

## 11. 测试

- `wind-theme`：皮肤 schema 解析、横竖 / 嵌入回退、base 整表替换。
- `wind-ui`（P3 后 `wind-scene`）：切片轴映射纯函数（固定段、多带比例分配、round 取整、最小尺寸）、
  装饰外包围盒与 margin、对齐点换算；皮肤 golden（程序生成的夹具图，入 `testdata`）。
- 出厂主题 golden 必须不变（皮肤是新增路径）。

## 12. P1 原型落点

| 内容 | 位置 |
|---|---|
| schema、求值、base 整表替换 | `wind-theme/src/skin.rs`（`[skin]` 进 `RESERVED_TOP`，不被 normalize 收进 views） |
| 轴映射、面积平均、双线性合成（纯函数） | `wind-ui/src/skin.rs` |
| 图集与皮肤位图缓存（建图集后丢源图，计入原有预算与闲置回收） | `wind-ui/src/image_cache.rs`（`skin_fill` / `skin_metrics`） |
| 候选窗接入：几何、外扩布局、绘制、对齐点 | `wind-scene/src/candidate/skin_geom.rs`（事件接线、装饰不拖动在 `wind-ui/src/candidate_window/skin_frame.rs` 与鼠标层） |
| 本机出图 | `cargo run -p wind-ui --features linux-host --example skin_preview -- <主题目录> <主题名> <out.png> [缩放] [vertical]` |

验收：本地用第三方位图皮肤（不入库）按 §1 的格式换算参数，横竖排 1x / 1.5x / 2x 目视还原；夹具测试钉住
最小尺寸、外扩、像素落点、装饰不拖动、竖排不回退横排。出厂主题 golden 不变。

常见格式换算：每轴「模式, 前固定, 后固定」→ `stretch_* = [[前, 图长 − 后]]`；编码区的上边 + 候选区的下边 +
两区的左右即 `content` 四边。

拉伸带要落在外轮廓恒定的一段：带内轮廓（如渐细的飘带尾）一变，`round` / `tile` 每次重复都会带一截斜边，
接起来成锯齿。编辑器（§9）按「带内每行最外侧不透明像素的位置是否恒定」给出警告。

P1 已知局限（审查后保留）：
- 「装饰区不拖动」只接在 Windows 的鼠标层；macOS / Linux 的拖动在前端进程，P2 随 `pass` 一起下发装饰矩形。
- 窗体位图按精确宽高进缓存，与普通填充共用 16 MiB 预算：逐键变宽时会更早触发整表清空。P4 做动画底图缓存时
  一并改成「按拉伸带分块缓存、绘制时拼」。
- 源图尺寸缓存（`dims`）只随闲置回收或预算清空失效；同路径换图（主题热改）可能沿用旧尺寸，切主题即恢复。

P1 未做、明确留给后续的：P2 `pass` 穿透、`avoid_caret`、`clamp`、平移进 `place_window`、
macOS 系统阴影与 Wayland 输入区；P3 起的共享 crate / 设置端 / 编辑器；P4–P5 动画。

## 13. P2 进度

已做（Windows）：
- 对齐点支持 `{ anchor, offset }`，并入 `place_window`；`avoid_caret`（缺省开，按像素保护带）、`clamp = "all"`。
- 拉伸带缺省填法改为 `round`。
- `hit = "pass"`：本地窗口路径把穿透装饰画进第二个 `WS_EX_TRANSPARENT` layered 窗口（类名
  `WindInputCandidateDecor`），与候选窗同位置同尺寸；穿透装饰全在窗体下（z < 0）时插到候选窗下面，否则在上面。
  拖动时一起挪，隐藏时一起收。窗口建不出来就回落为画进候选窗（按 block）。
- host-render（DLL 侧 HostWindow）、macOS、Linux 仍把穿透装饰画进主缓冲，按 block 处理。

未做：macOS 第二个 panel / 系统阴影、Wayland 输入区、X11 XShape、mac/Linux 的「装饰不拖动」——都要改前端进程，
随帧协议下发装饰矩形；穿透窗口与候选窗混合 z 序（同时有窗体上、下两类穿透装饰时统一放上面）。

## 14. P1b 落点

- schema：`wind-theme/src/skin.rs` 的 `SkinSlots` / `RvSlots`。写了 preedit 或 footer 却没写 candidates 时，
  记一条警告，按 stack 布局处理。
- 组装：`build_tree` 把编码栏、候选块、翻页栏经 `free_parts` 交出，根容器只是按窗体尺寸撑开的空盒。
- 求解与摆放：`wind-scene/src/candidate/skin_geom.rs` 的 `solve_axis`（`AxisFit` 带所锚框的上下界）、`skin_place_parts`。
  绘制和命中由 `paint_parts`、`collect_part_hits` 完成，候选点击沿用原有标签。
- 三条渲染路径（非 Windows `show`、ext_presenter、Windows 本地窗口）都接入了；free 时跳过上翻重建。

## 15. P4 范围（动画运行时）

本期只做「装饰按文件自带帧循环播放」，状态机与输入事件（§6.2 的 `states` / `on`）留给 P5。

- schema：`decor.fps` 可选，覆盖文件自带帧时长。其余动画字段 P5 再加。
- 解码：装饰图是 APNG / GIF / 动态 WebP 时按帧解码，逐帧用 §5 的面积平均缩到目标倍率；静态图仍是一帧。
  帧时长 < 10 ms 按 100 ms（浏览器惯例），帧率上限 30。
- 预算：每个主题的动画帧合计 24 MiB，超出则隔帧丢弃（时长相应累加）直到落入预算，并 warn。
- 时间轴：显示时记起点，当前帧 = 经过时间对总时长取模，不逐帧累加，不漂移；窗口隐藏即停（不登记唤醒）。
- 驱动：候选窗 `next_deadline` 登记下一次换帧时刻，`tick` 到期后由 UI 循环重绘一帧（本地窗口与
  host-render 两条路径都要）。本期整帧重绘，先在靶机量 CPU，不够再做「静态底图缓存 + 只合成装饰」。
- 减少动态效果：Windows `SPI_GETCLIENTAREAANIMATION` 关闭时停在首帧。设置项「关闭主题动画」放到 P5 一起做
  （要动设置端）。
- 平台：本期只在 Windows 播放；macOS / Linux（ext_presenter）先显示首帧，SHM 推帧放到后续。

## 16. P5 范围（事件状态机）

本期把 §6.2 的 `states` / `on` 接上输入事件，并加「关闭主题动画」设置项。平台同 P4，只在 Windows 播放。

声明（每个装饰各自一台状态机，互不影响）：

```toml
[skin.horizontal.decor.states]
idle   = { frames = "0-23" }               # 片段：帧区间（含两端，按文件原始帧号）
typing = { frames = "24-35" }
en     = { image = "tiger_en" }            # 片段：另一个文件，整段
[skin.horizontal.decor.on]
typing  = { state = "typing" }             # 切换常驻状态
idle_5s = { state = "idle" }
commit  = { play = "36-47", then = "idle" }  # 一次性片段，播完转 then
page    = { play = "48-55" }               # then 缺省 = "@prev"（回到当前常驻状态）
click   = { play = { image = "tiger_jump" } }
mode_en = { state = "en" }
```

- 没写 `states` 的装饰行为同 P4：整个文件循环（`on` 里的一次性片段照样可用）。写了则初始状态取 `idle`，
  没有 `idle` 取按名字排序的第一个（toml 表不保留书写顺序）。
- 常驻状态循环播放；一次性片段播一遍，播完转 `then`；播放中来了新事件，按新事件直接切换。
- 帧号指文件的原始帧号；预算降帧（隔帧丢弃）后按原始帧号映射到保留下来的帧。
- 引用了不存在的状态 / 越界帧区间：加载时 warn 并丢弃该条，不影响其它条目。
- 窗口隐藏时：`state` 类事件照样改常驻状态（下次显示生效），一次性片段丢弃、直接落到它的 `then`
  （写了的话；上屏多半伴随收窗，`commit = { play = .., then = "idle" }` 靠这条回到 idle）。显示时片段从头播。
- 静态图当片段时算一帧 1 秒：一次性片段显示 1 秒后转 `then`。
- 同一个常驻状态再次触发不从头播（连续打字不每键重播）；一次性片段再次触发从头播。
- 首键时候选窗还没显示：`typing` 的 `state` 照样生效，但 `typing` 的 `play` 首键看不到，从第二键起才播。
- 上屏通常紧跟收窗（`commit` 晚于收窗到达），`commit` 的 `play` 多半看不到：`commit` 宜写 `state`，
  或靠 `then` 落到想要的状态。
- `decor.fps` 同样作用于 `{ image = .. }` 引用的文件（一块装饰一个帧率，有意如此：同一角色的各段动作节奏一致）。
- 片段引用的额外动图也计入帧预算：本变体所有要解帧的文件均分 24 MiB，引用越多每份越小、越容易降帧。
- 帧区间按原始帧号映射到降帧后保留的帧：起点向上取整，区间里没有保留帧时取终点所在那帧；
  有帧解码失败（保留帧前移、映射对不上）时帧区间片段按整段播并 warn。
- 片段文件读不出（不存在 / 坏图）：该片段当作没写并 warn。
- `typing` / `backspace` / `empty` 只认「打字中」的帧（`UpdateCandidates.composing`，有编码且不在联想态）：
  上屏后进联想、编码清空都不报事件。
- 系统动画效果或 `ui.theme.animation` 让动画冻结 / 恢复时记一条 info 日志（只在变化时）。

事件来源：

| 事件 | 来源 |
|---|---|
| `show` | 候选窗从隐藏到显示 |
| `hide` | 候选窗从显示到隐藏（此时只有 `state` 与 `then` 生效） |
| `typing` / `backspace` | 相邻两次 `UpdateCandidates` 的编码串：变短且为原串前缀 = backspace，其余变化 = typing |
| `page` | 编码串不变、页码变化 |
| `empty` | 有编码但候选变为空 |
| `commit` | 协调器上屏出口真有文本上屏时发 `UiCommand::SkinSignal`：挂在 `apply_auto_spacing`（按键、点选，以及同样经它的气泡上屏与 CapsLock 钩子） |
| `mode_cn` / `mode_en` | UI 侧比较相邻两次 `UpdateToolbar` 的 `chinese_mode`（首次不算），不另加命令 |
| `idle_<N>s` | 显示中、距最后一个事件 N 秒（登记 `next_deadline`） |
| `click` | 在 `drag` / `block` 装饰上按下、光标未超过系统拖动阈值（`SM_CXDRAG` / `SM_CYDRAG`）即松开 |

`select`（数字键 / 点击选词）本期不单列：它总伴随 `commit`，主题想区分时再加。

关闭：`ui.theme.animation`（缺省 `true`）。关掉或系统关了动画效果时，装饰停在初始状态首帧，事件不再切换。
设置端在「外观 → 主题」加开关「主题动画」。
