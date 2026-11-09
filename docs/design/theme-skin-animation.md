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
idle   = { frames = "0-23" }
typing = { frames = "24-35" }
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
  macOS `accessibilityDisplayShouldReduceMotion`、Linux 设置门户 / kdeglobals，见 §17.1），关闭时停在 `idle` 首帧。
- 驱动：在 `manager.rs` 的 `next_deadline` 清单登记下一帧时刻。
- 重绘：静态部分（窗体 + 文字）缓存成一张底图，换帧只重新合成装饰矩形；P2 拆窗后只刷装饰窗口。
- macOS / Linux 走 SHM 推帧，帧率减半（取舍见 §17.1）。

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
- host-render（DLL 侧 HostWindow）：DLL 声明能力后同样拆到第二个穿透窗口，见 §17.8；旧 DLL 仍按 block。

macOS / Linux 前端（推帧动画、macOS 第二个 panel 与系统阴影、Wayland 输入区、X11 XShape、「装饰不拖动」）
已按 §17 实现；Swift 部分本机编不了，只经 CI 与真机验。
未做：穿透窗口与候选窗混合 z 序（同时有窗体上、下两类穿透装饰时统一放上面）。

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

## 17. P6 Unix 前端（macOS / Linux）

补齐 §13「未做」里前端进程那一半。macOS 与 Linux 都走 ext_presenter：服务进程光栅化候选窗写 SHM，
push `CMD_HOST_RENDER_FRAME`（几何 + flags）与 `CMD_CANDIDATE_RECTS`（命中矩形），前端（`.app` /
Fcitx5 addon）只贴图、落位、转发鼠标。盘点基准 `9f165489`：

| 项 | 现状 |
|---|---|
| 动画 | 只显示首帧：非 Windows 的 `system_allows_animation` 恒为 false；forwarder 不 `tick`、不登记换帧；收起时不调 `win.hide()`，P5 的 `hide` 事件与时间轴起点在这条路上都不对 |
| 穿透装饰 | 按 block 画进主缓冲 |
| 装饰不拖动 | macOS：命中表外一律起拖。Linux：候选窗本来就不能拖（`wind_linux/AGENTS.md`「候选窗拖动 未接」），装饰不在命中表里，点上去既不选词也不悬停，已等同 block |
| 输入区 | X11 ARGB 路径候选窗整个矩形接鼠标（浮层才按 alpha 设 `SK_INPUT`），皮肤的透明外扩区也挡点击；无合成器路径按 alpha≥128 抠 `BOUNDING`。Wayland 是候选窗整块矩形。macOS 非不透明窗口按 alpha 自动穿透 |
| macOS 阴影 | `hasShadow = true`，按整窗 alpha 投影：皮肤自带阴影，再叠一层系统阴影，装饰也被投影 |

### 17.1 动画：协议推帧，不做前端本地播放

| | 推帧（采用） | 前端本地播放 |
|---|---|---|
| CPU | 每帧服务端整帧重排重绘（Windows P4 实测 ≈0.3%），前端多贴一次图 | 前端只合成装饰矩形，最省 |
| 带宽 | 15 帧/秒 × 典型 600×240×4 ≈ 8.6 MB/s 本机内存拷贝；Retina 2× 为 4 倍。SHM 上限 4 MB 不变 | 加载时一次性传帧集（每主题至多 24 MiB） |
| 实现量 | 服务端接上现成的 `next_deadline` / `tick`；前端只认一个新 flag | 底图要拆三层（z < 0 / 窗体与内容之间 / ≥ 100），帧集要另开 SHM 段；状态机（§16 的事件、`idle_<N>s`）在服务端，前端还要收状态切换；C++ X11、C++ Wayland、Swift 三份实现 |

结论：推帧，帧率上限 **15**（Windows 上限 30 的一半）。时间轴仍是经过时间对总时长取模（§15），
限帧只是跳过中间帧，不漂移。只在闸门处取样会混叠：周期恰为 66 ms 整数倍的快循环（如两帧各 33 ms 的
眨眼）回回采到同一帧，看起来不动。所以唤醒时刻取「闸门之后第一个会换成不同帧号的边界」：推完一帧后，
候选窗按刚推出去的帧号，在各装饰状态机的副本上逐个边界试推，帧号相同的边界跳过（`next_anim_change`）。
上例因此 A、B 交替，只是每帧停留约 99 ms。靶机 CPU 不达标再评估本地播放，本期不做。

- forwarder 每轮 `win.tick()`，主循环睡到 `win.next_deadline()`；macOS 的主循环也改成按 deadline 醒
  （原先只等命令）。闸门在候选窗里（`set_anim_gate`，推完一帧才起算，相邻两帧推送间隔 ≥ 66 ms），只约束
  换帧到期；悬停激活、提示保持、隐藏后释放像素照常到期。
- 换帧走 `repaint_anim_frame`：不重推 tooltip、不重报候选窗矩形给状态气泡。
- 帧 flags 新增 `FLAG_ANIM_FRAME = 0x0010`：内容没变、只换装饰像素。前端照原位贴图，不重新落位、
  不重排 z 序（X11 不 raise——否则每帧压住 tooltip 与菜单；macOS 不 `invalidateShadow`）。旧前端忽略此位，
  按普通帧处理。
- 候选窗收起时调 `win.hide()`：动画停（不登记唤醒），`hide` 事件照 §16 生效，下次显示时间轴从头。
- 减少动态效果：macOS 由 `.app` 读 `NSWorkspace.accessibilityDisplayShouldReduceMotion`，经扩展信封
  `host.reduce_motion`（`{"reduce":bool}`）报给服务——连上服务、服务重启、系统设置变化
  （`accessibilityDisplayOptionsDidChangeNotification`）时各报一次；服务进程不链 AppKit，以上报值为准，
  没收到时退回 CFPreferences 读 `com.apple.universalaccess` 的 `reduceMotion`（先 `AppSynchronize`，
  `valid == 0` 按未知、允许）。下一次显示候选窗时生效。
- Linux 同样由宿主（Fcitx5 addon，`DesktopMotion`）读、经同一个 `host.reduce_motion` 报，服务端没收到按允许。
  来源与取舍：
  - **设置门户**（`org.freedesktop.portal.Settings`）：`org.freedesktop.appearance` `reduced-motion`
    （跨桌面的新键，1 = 减少）、`org.gnome.desktop.interface` `enable-animations`、`org.kde.kdeglobals.KDE`
    `AnimationDurationFactor`（0 = 关）。走 Fcitx5 自带的 DBus 封装（`fcitx-utils/dbus`，挂在 Fcitx5 事件
    循环上、异步），不引入新依赖，也不像 gsettings / dconf 那样要起进程或链 GLib；`SettingChanged`
    信号让设置改了即时生效。Fcitx5 编译时关了 DBus（头文件不在）就只剩下一条。
  - **kdeglobals 文件**（`$XDG_CONFIG_HOME/kdeglobals` 的 `[KDE] AnimationDurationFactor`）：没有门户的
    KDE 会话也读得到；只在 addon 起来与服务（重）启时读，改了要等下一次。组名 / 键名尾部的 KConfig 修饰
    （`[KDE][$i]` 锁定等）剥掉再比，开头的 UTF-8 BOM 忽略。**只读用户层**：系统层（`$XDG_CONFIG_DIRS` 下的
    kdeglobals，管理员下发的默认值 / 锁定）不读——要按 KConfig 的层叠与 `$i` 锁定语义合并才对，为一个
    动画倍率不值得；有门户的 KDE 会话门户给的已是合并后的值，缺的只是「没有门户 + 只在系统层设了」这种情形。
  - 挑选（`pickReduceMotion`）：`reduced-motion` 有值以它为准；否则 `XDG_CURRENT_DESKTOP` 含 `KDE` 时先 KDE
    （门户优先于文件）后 GNOME，别的桌面先 GNOME 后 KDE——GNOME 会话里留着 KDE 应用写的 kdeglobals、
    KDE 会话里 GNOME 的键是 gsettings 出厂值，都不能代表本桌面的设置。都读不到不报。
  - 不做：直接读 dconf 数据库（GVariant 数据库格式，要自己解析）、调 `gsettings`（起进程，且 Flatpak /
    非 GNOME 环境常常没有）。没有门户的 GNOME 会话（极少）因此按允许。
  - 门户在 addon 首读时可能还没起来（D-Bus 按需拉起，或桌面晚于 Fcitx5）：信号照收；读的超时 5 秒，没回音
    （NoReply）再试两次；门户上线（`ServiceWatcher` 见到 `org.freedesktop.portal.Desktop` 有了属主）与服务
    （重）启时向门户重读一轮；每次连上服务都按现值补报一次（断开期间变了的不丢）。某个键读失败（门户不认 /
    一直没回音 / 回包不是 variant）就当这一路没读到，不留上一个门户给的旧值。回包与信号先核整条消息的签名
    （`Read` 回包 `v`、`SettingChanged` `ssv`）再读：Fcitx5 的 sd-bus 封装对基本类型 peek 会抛异常，在 DBus
    回调里就是整个 fcitx5 退出。

### 17.2 协议：`CMD_CANDIDATE_SKIN`（0x0514，push）

每个候选帧（含换帧）在 `CMD_HOST_RENDER_FRAME` 之前推一条，没启用皮肤时也推（`flags = 0`、两表为空），
前端据此复位。先到是为了让前端贴这一帧时就用上这一帧的输入区与阴影设置，不必等下一条消息再改。
两端前端对未知 cmd 都落 `default` 安静忽略，旧前端不受影响。坐标同 `CMD_CANDIDATE_RECTS`：
缓冲内的设备像素，macOS / Wayland 前端除以 scale。

```
flags     u32      bit0 ACTIVE 本帧启用了皮肤；bit1 MASK 带输入区表
frame     4×i32    窗体矩形 x, y, w, h
decorN    u32
decorN ×  { index u32   变体 decor 下标（与 §16 状态机同号）
            hit   u32   0 drag | 1 block | 2 pass
            flags u32   bit0 UNDER（z < 0）；bit1 SPLIT（穿透装饰画在独立层，不在主缓冲）
            x, y, w, h  4×i32 }                            每条 28 字节
maskN     u32
maskN ×   { x, y, w, h  4×i32 }                            输入区矩形
```

- **输入区**（只 Linux 发，`MASK` 置位）：主层（不含穿透装饰）alpha > 0 的像素——与 Windows layered
  窗口的命中口径一致。逐行取不透明游程成矩形，相邻行游程完全相同的纵向合并；圆角窗体约几十个，
  人物装饰按其高度每行一两个。超过 **1024** 个时退化为「窗体矩形 ∪ 非穿透装饰矩形」（仍置 `MASK`，
  表即退化结果），并 debug 一条。
- 穿透装饰在 Linux 仍画进同一张位图（按 Windows 的统一 z 规则：全在窗体下则垫在主层下面，否则叠在上面），
  只是不进输入区。macOS 拆到独立层（`SPLIT`），见 §17.3。
- 不拖动的判定在前端做：`block` 装饰矩形（及没拆出去的 `pass`）内、窗体矩形外 → 不起拖。`drag` 装饰照常起拖。
- 装饰点击（§16 的 `click`）也在前端判，判完报上行扩展信封 `skin.decor_click`（`{"index":N}`，N 即上表的
  `index`），服务端经 `UiCommand::SkinDecorClick` 交给候选窗触发该装饰的 `click`（`skin_decor_click`，窗口收起时
  丢弃）。判定与 Windows `CandidateMouse` 一致：先判候选 / 翻页按钮命中，命中了就不算装饰；否则取点下的
  最上层 `drag` / `block` 装饰（表按 z 升序倒着找，`pass` 不算，窗体内的 `UNDER` 装饰被窗体盖住也不算）；
  松开时仍在同一块上、各轴位移不超过拖动阈值才报（Linux 取 4 px，同 Windows 出厂的 `SM_CXDRAG`；macOS
  沿用本端起拖的 3 pt）。两处已知的细小差异，不改：装饰矩形的命中 Linux / macOS 按半开区间（右 / 下边那一行
  像素不算），Windows `Rect::contains` 是闭区间，边上差 1 像素；阈值 Linux、Windows 是「不超过」（≤ 4），
  macOS 是「严格小于」3 pt。服务端收到后按当前帧核对：`index` 不是这一帧里可点的装饰（越界、`pass`、宿主
  按换皮肤前的旧帧报的）就丢掉。纯函数：Linux `decorAt` + `DecorClickGate`，macOS `CandidateSkin.decorAt`。
  旧服务不认这个 kind，`handle_ext` 落 `_` 分支安静忽略；新服务 + 旧前端只是没人报，`click` 不触发。

### 17.3 各平台

**X11**（`X11Panel`）
- 收到 `MASK`：候选窗 `SK_INPUT` 设为输入区表（ARGB 与无合成器两条路都设；无合成器时 `BOUNDING` 照旧按
  alpha ≥ 128 抠形，生效输入区是两者之交）。之后没有 `MASK`（换回普通主题）就把 `SK_INPUT` 复位成缺省
  （`xcb_shape_mask(..., XCB_NONE)`），恢复整窗接鼠标。窗口重建（合成器起停换 visual）后补设一次。
- 换帧（`FLAG_ANIM_FRAME`）：沿用上次落位，`present` 不 raise。
- 候选窗拖动（§17.7）：空白处与 `drag` 装饰起拖，窗体外的 `block` / `pass` 装饰不拖。

**Wayland**（`WaylandPanel`）
- 输入区 = 输入区表 ÷ scale，另并上状态气泡矩形（现状）；没有 `MASK` 时照旧是候选窗整块矩形。
  `wl_surface.set_input_region` 是核心协议，所有合成器都认；KWin 的 input-method-v1 面板走同一条
  `attachPixels`。popup 由合成器摆位，不能拖（§17.7）：`drag` 装饰在原生 Wayland 应用里与 `block` 一样。

**macOS**（`CandidatePanel`）
- 穿透装饰拆到独立层：服务端主缓冲不画 `pass`（主层 alpha 为 0 处窗口天然穿透），穿透装饰另画一张同尺寸
  位图，写 SHM 段 `_DEC`、推 `CMD_OVERLAY_FRAME`（新层 `OVERLAY_KIND_DECOR = 12`，`OVERLAY_PLACE_EXACT`，
  坐标即候选窗建议落点）。`.app` 不交给 `OverlayHost`，而是挂在候选窗上的子 panel
  （`addChildWindow(_:ordered:)`，全在窗体下用 `.below`，否则 `.above`）：`ignoresMouseEvents = true`、
  `hasShadow = false`、同原点同尺寸，随候选窗拖动与显隐。本帧没有穿透装饰就推该层的隐藏帧。
- 系统阴影：`ACTIVE` 时主 panel `hasShadow = false`（皮肤阴影由图自带，§3.1），否则照旧开。
- 装饰不拖动：`mouseDown` 落在不拖动区（§17.2）不起拖，按下与松开都不动作。

### 17.4 降级

| 情形 | 行为 |
|---|---|
| X11 无 SHAPE 扩展 | 输入区设不了，整窗接鼠标：穿透装饰按 block，透明外扩区也挡点击（与现状同），启动时记一条 |
| X11 无合成器 | `BOUNDING` 按 alpha ≥ 128（半透明退化，现状），`SK_INPUT` 照设 |
| 输入区矩形超上限 | 退化为矩形并集（§17.2） |
| macOS 子 panel 来不及收到像素 | 主层不含穿透装饰，最坏是那一帧看不到穿透装饰，不会挡点击 |
| 旧前端 + 新服务 | 新 cmd、新 flag、新层都被忽略：Linux 穿透按 block；macOS 看不到穿透装饰（主层已不画它）——`.app` 与服务同包发布、须一起装，不另做兼容 |
| 新前端 + 旧服务 | 收不到新 cmd，行为同改前；上报的 `skin.decor_click` 被当未知扩展忽略 |

### 17.5 测试

- Rust 原生单测：`wind-ipc` 的 `encode_candidate_skin` 字节布局；`wind-scene` 输入区游程合并（圆角、
  纵向合并、上限退化、穿透装饰不进表）；`wind-ui`（`linux-host` feature 下的 forwarder 单测）：皮肤帧带
  `CMD_CANDIDATE_SKIN`，普通主题 `flags = 0`；动画按 deadline 推帧、帧数不超过限帧上限、闸门内空转
  不出帧、帧带 `FLAG_ANIM_FRAME`、换帧不重推 tooltip、收起后不再出帧；皮肤区域先于候选帧。
- C++ 单测（`make -C wind_linux test`）：`codec_test` 按 Rust 编码器写死的偏移解码；`protocol_sync_test` 钉新
  cmd / flag / 层；输入区换算纯函数（无皮肤回退整窗、Wayland 缩放、空表）。
- e2e（本机 Xvfb，`scripts/linux/e2e.sh`）：程序生成的皮肤主题（窗体 + 右上外侧穿透装饰 + 左上外侧
  block 装饰），打字后 `xdotool mousemove` 到穿透装饰上，`getmouselocation` 报的窗口不是候选窗，挪到
  窗体上是候选窗；动图装饰（程序生成的两帧 APNG）在候选窗显示期间 `xwd -id` 两次截图不同；`xdotool click`
  点 block 装饰，它的 `click` 切了状态（换色）、不上屏、候选窗不收。装饰点击的命中与闸门另有 C++ / Swift 单测，
  服务端收到 `skin.decor_click` 的路由有协调器单测。同组另起一个扮演设置门户的 DBus 服务（接过私有会话
  总线上被按需拉起的真门户的名字）：发 `SettingChanged` 关动画后再显示，动图装饰不再换色，开回来恢复；
  服务重启后靠 addon 重读门户（`Read`，两层 variant）再报。挑选规则与 kdeglobals 解析是 C++ 单测。
- macOS：Rust 部分 `cargo check --target aarch64-apple-darwin`；Swift 本机编不了，只能 CI 的 macOS job
  与真机，判据见报告。
- 候选窗拖动（§17.7）：C++ 单测拖动阈值、不拖动区、锚的优先级、候选帧 36 字节解码；Rust 单测固定位置下
  「帧坐标 + 内容偏移」回到 `custom_x/y`、帧长 36 字节；e2e（Xvfb，两条透明路径）按住空白处 / `drag` 装饰
  拖动窗口跟手、`block` 装饰上拖不动、阈值内松开不算拖、续帧不弹回、收起后回到光标旁、切固定位置时应答
  查询落盘内容左上、固定位置下拖动落盘且重显不漂。
- 关键逻辑在 `git archive` 导出的副本上做变异验证。

### 17.6 本期不做

- 前端本地播放动画（§17.1）。

### 17.7 Linux 候选窗拖动

目标与 Windows `CandidateMouse` 一致：空白处（编码栏、内边距）与 `drag` 装饰起拖，`block` 装饰（窗体外）
不拖、只判点击，候选 / 翻页按钮照旧点选。改前 Linux 候选窗不能拖，`drag` 装饰等同 `block`。

**平台能不能做**

- X11：候选窗是 override-redirect 窗口（不抢焦点、不加边框），不归窗口管理器管，`_NET_WM_MOVERESIZE`
  对它无效（那是请 WM 代拖受管窗口）。可行的是本端拖，同状态气泡现有的做法：左键按下记锚点，按下
  带来的隐式抓取保证指针移出窗口仍收到移动；挪过阈值（4 px）后 `xcb_configure_window` 跟随；松手报
  `pos.candidate`。没过阈值就松开，按 §17.2 判 `drag` 装饰点击（`DecorClickGate` 已有）。
- Wayland：候选窗是 input-method-v2 的 popup surface（KWin 是 input-method-v1 的 input panel），由合成器
  按文本光标摆位，协议里没有移动请求；`xdg_toplevel.move` 只对 toplevel 有效，不适用。**不支持**。
  XWayland 应用里的候选窗走 X11 那条，可以拖——同一会话里两类应用行为不一，需要维护者认可。

**落位语义要补的三件**

1. 本次组合内的拖动锚（drag pin）：Windows、macOS 都由前端记，组合内后续帧（翻页、继续输入）按锚摆，
   收起清掉；否则下一帧就被服务端的建议点拽回光标旁。X11 `show()` 现在每帧按建议点 `placePanel`，要加
   一级：锚 > 固定位置（`FRAME_FLAG_ABSOLUTE_POS`）> 跟随光标；跟随候选窗的浮层（悬停提示）的平移量
   `candShift` 随之按锚算。
2. 上报坐标：`pos.candidate` 约定是**内容**左上（`ui.candidate.custom_x/y` 同义，Windows 报窗口左上 +
   阴影扩边）。Linux 候选帧（`CMD_HOST_RENDER_FRAME`）只有整幅位图的位置与尺寸，X11 无合成器时软阴影
   画在位图里，本端换算不出内容左上。两条路：
   - (a) 候选帧尾追加 `contentX` / `contentY`（同 `CMD_OVERLAY_FRAME` 的字段；旧 addon 按长度只读前段，
     新 addon 收旧服务的短帧按 0）。语义自包含，不依赖服务端记住哪一帧。
   - (b) 服务端对 Linux 宿主把 `pos.candidate` 当窗口左上，用最近一帧的扩边换算。协议不动，但同一个 kind
     在两个平台上含义不同。
   倾向 (a)。
3. 「固定位置」切换时服务端问当前位置（`pos.candidate.query`）：macOS 应答；Linux 有了锚之后服务端不知道
   窗口实际在哪，addon 要同样应答（现在只答 `pos.status_tip.query`，见 wind_linux/AGENTS.md 的差距表）。

**测试**：C++ 单测拖动判定（阈值、按在候选 / block 装饰上不起拖、锚的优先级）；e2e（Xvfb，`xdotool`
按住空白拖动）断言窗口跟随、继续打字不弹回、收起后回到光标旁；固定位置模式下松手后 `custom_x/y` 落盘为
内容左上；`block` 装饰上按住拖不动、`drag` 装饰上能拖。

**定案与实现**（维护者定：走 (a)；原生 Wayland 应用不支持可以接受）

- 协议：`CMD_HOST_RENDER_FRAME` 由 28 字节加长到 36 字节，尾部 `contentX` / `contentY`（i32）= 窗口左上到
  内容左上的偏移。服务端取 `(ml, mt − clear)`（与 `render_frame` 摆窗口时减掉的同一组量；外部宿主这条路
  不画软阴影，实际只有装饰外扩，`clear` 恒 0），故固定位置下「帧坐标 + 偏移」正好是 `custom_x/y`，「拖动 → 落盘 → 重显」不漂。macOS `.app`
  与旧 addon 按长度只读前 28 字节；新 addon 收旧服务的短帧按 0。
- X11 / XWayland（`X11Panel`）：按下时排除候选 / 翻页按钮与不拖动区（`onNoDragDecor`：窗体外的 `block` /
  `pass` 装饰矩形），其余 `CandidateDragGate.press`；挪过 4 px 后跟随（`xcb_configure_window`，fleur 光标，
  悬停清掉、提示随之收起），窗口夹进工作区。没过阈值的松开不算拖：窗口不动、不报，装饰点击照判。
  拖动锚记内容左上，`placeCandidate` 按「锚 − 本帧偏移」摆、只钳制不翻转；`candShift` 按实际落点算，
  跟随候选窗的浮层随之平移。候选窗收起（隐藏帧）清锚。
- 上报与应答：松手报 `pos.candidate`（内容左上）；引擎应 `pos.candidate.query` 报 X11 候选窗当前内容
  左上，候选窗不在屏上（含原生 Wayland 应用那条路）不答，服务端保留旧值。
- **限制**：原生 Wayland 应用的候选窗不能拖，也不答查询；同一会话里 XWayland 应用能拖、原生应用不能。
  拖动夹回按整块根窗口的工作区，不分显示器（同状态气泡）。不跟 Windows 的「空白处双击截图」。

### 17.8 Windows DLL host-render 的穿透装饰

SearchHost 这类宿主里的候选窗是 DLL 在宿主进程里用 `CreateWindowInBand` 建的 band 窗口（`CHostWindow`），
像素由服务端经 SHM 推。原先服务端把穿透装饰画进主层，整块接鼠标（按 block）。现在与本地窗口路径
（`WindInputCandidateDecor`）同一做法：

- **能力协商**：`CMD_HOST_RENDER_REQUEST` 原本没有载荷，新 DLL 带 4 字节能力位（`HOST_RENDER_CAP_PASS_LAYER = 1`）。
  旧服务不读载荷，照旧回 setup；新服务按连接记下（`HostRenderTarget.pass_layer`）。只给声明了能力的 DLL 发拆层帧。
- **帧格式**：`SharedRenderHeader` 的保留字 @56 改作 `pass_offset`；帧带 `FLAG_PASS_LAYER = 0x20`（`FLAG_PASS_BELOW = 0x40`
  表示装饰全在窗体下）时，与主层同尺寸的穿透层追加在矩形表之后。主层不含穿透装饰（透明处天然穿透）。
- **DLL**：候选 kind 建窗时多建一个同 owner、同 band 的 `WS_EX_TRANSPARENT` layered 窗口，每帧与主窗口同位置同尺寸贴图；
  BELOW 时紧贴候选窗下面，否则 `HWND_TOPMOST` 置于其上。没有穿透层的帧收起它，隐藏 / 换 band / 卸载时一起走。
- **退路**（都退成原先的按 block 显示）：
  - 旧 DLL + 新服务：没声明能力，服务端发合成好的单层。
  - 新 DLL + 旧服务：收不到拆层帧，同改前。
  - 穿透窗口建不出来（`CreateWindowInBand` 第二次失败）：DLL 把穿透层合回主层（`CompositePass`，与服务端
    `composite_over/under` 同一算式）再贴。
  - 两层放不下 SHM（4 MiB）：服务端合成单层重写。
- **做不到 / 不做**：
  - 窗体上、下两类穿透装饰同时存在时一律放上面（同本地路径，§13）。
  - 「在上」的穿透窗口是 band 内 `HWND_TOPMOST`，可能盖住同 band 的悬停提示 / 状态气泡（它们 owned 于候选窗）；
    本地路径同样如此。要按 owner 链精排得把三个窗口的建窗顺序一起改，本期不做。
  - band 窗口能否在受限宿主（AppContainer）里再建一个，只能靶机验证；建不出来走上面的退路，日志有一条 warn。
- **测试**：Rust 帧编码（偏移、上下位、尺寸不符不写、放不下报错）、能力位按连接记录（服务端分发 + 管理器，经 wine 跑）、
  拆层出帧（`render_host_frame`，`#[cfg(windows)]`，只在 Windows 上跑）；C++ 纯函数 `HostPassLayerPolicy.h`
  （偏移校验、越界 / 溢出、上下位、合成算式，与两边协议常量对拍），`g++` 本机跑。
